use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::cmp::Ordering as CmpOrdering;

#[derive(Clone)]
pub struct TunnelClient {
    pub remote_ep: Option<SocketAddr>,
    last_receive_tick: Arc<AtomicU64>,
    timeout_seconds: u64,

    // Performance tracking
    packet_count: Arc<AtomicU64>,
    bytes_received: Arc<AtomicU64>,
    bytes_sent: Arc<AtomicU64>,

    // Bandwidth estimation (bytes per second)
    bandwidth_estimate: Arc<AtomicU64>,
    last_bandwidth_calc: Arc<AtomicU64>,

    // Priority scoring (higher = more priority)
    priority_score: Arc<AtomicU64>,

    // Connection quality metrics
    latency_ms: Arc<AtomicU32>,
    packet_loss_rate: Arc<AtomicU32>, // 0-1000 (0.0% - 100.0%)

    // Connection state
    connection_start: Arc<AtomicU64>,
    is_slow_connection: Arc<AtomicU32>, // 0 or 1
}

impl TunnelClient {
    pub fn new(timeout_seconds: u64) -> Self {
        let now = Self::current_timestamp();
        Self {
            remote_ep: None,
            last_receive_tick: Arc::new(AtomicU64::new(now)),
            timeout_seconds,
            packet_count: Arc::new(AtomicU64::new(0)),
            bytes_received: Arc::new(AtomicU64::new(0)),
            bytes_sent: Arc::new(AtomicU64::new(0)),
            bandwidth_estimate: Arc::new(AtomicU64::new(0)),
            last_bandwidth_calc: Arc::new(AtomicU64::new(now)),
            priority_score: Arc::new(AtomicU64::new(1000)), // Default priority
            latency_ms: Arc::new(AtomicU32::new(0)),
            packet_loss_rate: Arc::new(AtomicU32::new(0)),
            connection_start: Arc::new(AtomicU64::new(now)),
            is_slow_connection: Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn new_with_endpoint(addr: SocketAddr, timeout_seconds: u64) -> Self {
        let now = Self::current_timestamp();
        Self {
            remote_ep: Some(addr),
            last_receive_tick: Arc::new(AtomicU64::new(now)),
            timeout_seconds,
            packet_count: Arc::new(AtomicU64::new(0)),
            bytes_received: Arc::new(AtomicU64::new(0)),
            bytes_sent: Arc::new(AtomicU64::new(0)),
            bandwidth_estimate: Arc::new(AtomicU64::new(0)),
            last_bandwidth_calc: Arc::new(AtomicU64::new(now)),
            priority_score: Arc::new(AtomicU64::new(1000)),
            latency_ms: Arc::new(AtomicU32::new(0)),
            packet_loss_rate: Arc::new(AtomicU32::new(0)),
            connection_start: Arc::new(AtomicU64::new(now)),
            is_slow_connection: Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn set_last_receive_tick(&self) {
        self.last_receive_tick.store(Self::current_timestamp(), Ordering::Release);
    }

    pub fn is_timed_out(&self) -> bool {
        let current = Self::current_timestamp();
        let last = self.last_receive_tick.load(Ordering::Acquire);
        (current.saturating_sub(last)) >= self.timeout_seconds
    }

    pub fn update_stats(&self, bytes_in: usize, bytes_out: usize) {
        self.packet_count.fetch_add(1, Ordering::Relaxed);
        self.bytes_received.fetch_add(bytes_in as u64, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes_out as u64, Ordering::Relaxed);

        // Update bandwidth estimate every second
        let now = Self::current_timestamp();
        let last_calc = self.last_bandwidth_calc.load(Ordering::Acquire);

        if now > last_calc {
            let time_delta = now - last_calc;
            if time_delta >= 1 {
                let total_bytes = self.bytes_received.load(Ordering::Relaxed)
                    + self.bytes_sent.load(Ordering::Relaxed);
                let bandwidth = total_bytes / time_delta;

                // Exponential moving average for smoother estimates
                let old_estimate = self.bandwidth_estimate.load(Ordering::Acquire);
                let new_estimate = (old_estimate * 7 + bandwidth * 3) / 10;

                self.bandwidth_estimate.store(new_estimate, Ordering::Release);
                self.last_bandwidth_calc.store(now, Ordering::Release);

                // Update priority based on new bandwidth
                self.update_priority();
            }
        }
    }

    pub fn update_priority(&self) {
        let bandwidth = self.bandwidth_estimate.load(Ordering::Acquire);
        let latency = self.latency_ms.load(Ordering::Acquire) as u64;
        let loss_rate = self.packet_loss_rate.load(Ordering::Acquire) as u64;
        let connection_age = Self::current_timestamp() - self.connection_start.load(Ordering::Acquire);

        // Priority calculation:
        // - Lower bandwidth = higher priority (help slow connections)
        // - Higher latency = higher priority
        // - Higher packet loss = higher priority
        // - Older connections get slight priority boost

        // Base score: inverse of bandwidth (capped)
        let bandwidth_score = 1_000_000u64.saturating_div(bandwidth.max(100) + 1000);

        // Latency factor: +1 point per ms of latency (capped at 500)
        let latency_score = latency.min(500);

        // Packet loss factor: +10 points per 1% loss
        let loss_score = (loss_rate * 10) / 10;

        // Age bonus: +1 point per 10 seconds (capped at 100)
        let age_score = (connection_age / 10).min(100);

        // Calculate final priority
        let priority = bandwidth_score
            .saturating_add(latency_score)
            .saturating_add(loss_score)
            .saturating_add(age_score);

        self.priority_score.store(priority, Ordering::Release);

        // Mark as slow connection if bandwidth < 100KB/s
        let is_slow = if bandwidth < 100_000 { 1 } else { 0 };
        self.is_slow_connection.store(is_slow, Ordering::Release);
    }

    pub fn get_priority(&self) -> u64 {
        self.priority_score.load(Ordering::Acquire)
    }

    pub fn set_latency(&self, latency_ms: u32) {
        self.latency_ms.store(latency_ms, Ordering::Release);
        self.update_priority();
    }

    pub fn set_packet_loss_rate(&self, rate: u32) {
        self.packet_loss_rate.store(rate.min(1000), Ordering::Release);
        self.update_priority();
    }

    pub fn is_slow_connection(&self) -> bool {
        self.is_slow_connection.load(Ordering::Acquire) == 1
    }

    pub fn get_connection_info(&self) -> ConnectionInfo {
        ConnectionInfo {
            endpoint: self.remote_ep,
            packet_count: self.packet_count.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            bandwidth_bps: self.bandwidth_estimate.load(Ordering::Relaxed),
            latency_ms: self.latency_ms.load(Ordering::Relaxed),
            packet_loss_rate: self.packet_loss_rate.load(Ordering::Relaxed),
            priority_score: self.priority_score.load(Ordering::Relaxed),
            connection_age_secs: Self::current_timestamp() - self.connection_start.load(Ordering::Relaxed),
            is_slow: self.is_slow_connection(),
            is_timed_out: self.is_timed_out(),
        }
    }

    fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs()
    }

    pub fn reset_stats(&self) {
        let now = Self::current_timestamp();
        self.packet_count.store(0, Ordering::Release);
        self.bytes_received.store(0, Ordering::Release);
        self.bytes_sent.store(0, Ordering::Release);
        self.bandwidth_estimate.store(0, Ordering::Release);
        self.last_bandwidth_calc.store(now, Ordering::Release);
        self.connection_start.store(now, Ordering::Release);
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub endpoint: Option<SocketAddr>,
    pub packet_count: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub bandwidth_bps: u64,
    pub latency_ms: u32,
    pub packet_loss_rate: u32,
    pub priority_score: u64,
    pub connection_age_secs: u64,
    pub is_slow: bool,
    pub is_timed_out: bool,
}

impl ConnectionInfo {
    pub fn bandwidth_kbps(&self) -> f64 {
        (self.bandwidth_bps as f64) / 1024.0
    }

    pub fn packet_loss_percent(&self) -> f64 {
        (self.packet_loss_rate as f64) / 10.0
    }
}

// Implement ordering for priority queue usage
impl PartialEq for TunnelClient {
    fn eq(&self, other: &Self) -> bool {
        self.remote_ep == other.remote_ep
    }
}

impl Eq for TunnelClient {}

impl PartialOrd for TunnelClient {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for TunnelClient {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // Higher priority score = process first
        other.get_priority().cmp(&self.get_priority())
    }
}

// Connection quality analyzer
pub struct QualityAnalyzer {
    samples: Vec<(u64, u32)>, // (timestamp, latency)
    loss_count: u32,
    total_count: u32,
}

impl QualityAnalyzer {
    pub fn new() -> Self {
        Self {
            samples: Vec::with_capacity(100),
            loss_count: 0,
            total_count: 0,
        }
    }

    pub fn add_latency_sample(&mut self, latency_ms: u32) {
        let now = TunnelClient::current_timestamp();
        self.samples.push((now, latency_ms));

        // Keep only last 100 samples or last 60 seconds
        let cutoff = now.saturating_sub(60);
        self.samples.retain(|(ts, _)| *ts > cutoff);

        if self.samples.len() > 100 {
            self.samples.drain(0..self.samples.len() - 100);
        }
    }

    pub fn record_packet(&mut self, lost: bool) {
        self.total_count += 1;
        if lost {
            self.loss_count += 1;
        }

        // Reset counters periodically to avoid overflow
        if self.total_count > 10000 {
            self.loss_count = self.loss_count / 2;
            self.total_count = self.total_count / 2;
        }
    }

    pub fn get_average_latency(&self) -> u32 {
        if self.samples.is_empty() {
            return 0;
        }

        let sum: u32 = self.samples.iter().map(|(_, lat)| *lat).sum();
        sum / self.samples.len() as u32
    }

    pub fn get_packet_loss_rate(&self) -> u32 {
        if self.total_count == 0 {
            return 0;
        }

        // Return as 0-1000 (0.0% - 100.0%)
        ((self.loss_count as u64 * 1000) / self.total_count as u64) as u32
    }

    pub fn get_jitter(&self) -> u32 {
        if self.samples.len() < 2 {
            return 0;
        }

        let mut diffs = Vec::with_capacity(self.samples.len() - 1);
        for i in 1..self.samples.len() {
            let diff = (self.samples[i].1 as i32 - self.samples[i - 1].1 as i32).abs() as u32;
            diffs.push(diff);
        }

        let sum: u32 = diffs.iter().sum();
        sum / diffs.len() as u32
    }
}