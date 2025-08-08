use prometheus::{IntCounter, IntGauge, Histogram, HistogramOpts, Encoder, TextEncoder};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

pub struct Metrics {
    // Tunnel V3 metrics
    pub v3_packets_received: AtomicU64,
    pub v3_packets_sent: AtomicU64,
    pub v3_bytes_received: AtomicU64,
    pub v3_bytes_sent: AtomicU64,
    pub v3_active_clients: AtomicUsize,
    pub v3_ping_requests: AtomicU64,

    // Tunnel V2 metrics
    pub v2_packets_received: AtomicU64,
    pub v2_packets_sent: AtomicU64,
    pub v2_bytes_received: AtomicU64,
    pub v2_bytes_sent: AtomicU64,
    pub v2_active_clients: AtomicUsize,
    pub v2_http_requests: AtomicU64,
    pub v2_ping_requests: AtomicU64,

    // P2P metrics
    pub p2p_requests: AtomicU64,
    pub p2p_responses: AtomicU64,

    // System metrics
    pub rate_limit_hits: AtomicU64,
    pub dropped_packets: AtomicU64,
    pub dropped_connections: AtomicU64,
    pub invalid_packets: AtomicU64,
    pub memory_usage: AtomicU64,
    pub cpu_usage: AtomicU64,

    // Performance metrics
    pub packet_processing_time_us: AtomicU64,
    pub forwarding_latency_us: AtomicU64,

    // Start time for uptime calculation
    start_time: Instant,

    // Prometheus metrics
    prom_v3_packets_received: IntCounter,
    prom_v3_packets_sent: IntCounter,
    prom_v3_bytes_received: IntCounter,
    prom_v3_bytes_sent: IntCounter,
    prom_v3_active_clients: IntGauge,

    prom_v2_packets_received: IntCounter,
    prom_v2_packets_sent: IntCounter,
    prom_v2_active_clients: IntGauge,
    prom_v2_http_requests: IntCounter,

    prom_rate_limit_hits: IntCounter,
    prom_dropped_packets: IntCounter,
    prom_dropped_connections: IntCounter,

    prom_packet_latency: Histogram,
    prom_uptime_seconds: IntGauge,
}

impl Metrics {
    pub fn new() -> Self {
        // Create Prometheus metrics
        let prom_v3_packets_received = IntCounter::new(
            "cncnet_v3_packets_received_total",
            "Total number of packets received by Tunnel V3",
        ).unwrap();

        let prom_v3_packets_sent = IntCounter::new(
            "cncnet_v3_packets_sent_total",
            "Total number of packets sent by Tunnel V3",
        ).unwrap();

        let prom_v3_bytes_received = IntCounter::new(
            "cncnet_v3_bytes_received_total",
            "Total bytes received by Tunnel V3",
        ).unwrap();

        let prom_v3_bytes_sent = IntCounter::new(
            "cncnet_v3_bytes_sent_total",
            "Total bytes sent by Tunnel V3",
        ).unwrap();

        let prom_v3_active_clients = IntGauge::new(
            "cncnet_v3_active_clients",
            "Number of active clients in Tunnel V3",
        ).unwrap();

        let prom_v2_packets_received = IntCounter::new(
            "cncnet_v2_packets_received_total",
            "Total number of packets received by Tunnel V2",
        ).unwrap();

        let prom_v2_packets_sent = IntCounter::new(
            "cncnet_v2_packets_sent_total",
            "Total number of packets sent by Tunnel V2",
        ).unwrap();

        let prom_v2_active_clients = IntGauge::new(
            "cncnet_v2_active_clients",
            "Number of active clients in Tunnel V2",
        ).unwrap();

        let prom_v2_http_requests = IntCounter::new(
            "cncnet_v2_http_requests_total",
            "Total number of HTTP requests to Tunnel V2",
        ).unwrap();

        let prom_rate_limit_hits = IntCounter::new(
            "cncnet_rate_limit_hits_total",
            "Total number of rate limit hits",
        ).unwrap();

        let prom_dropped_packets = IntCounter::new(
            "cncnet_dropped_packets_total",
            "Total number of dropped packets",
        ).unwrap();

        let prom_dropped_connections = IntCounter::new(
            "cncnet_dropped_connections_total",
            "Total number of dropped connections",
        ).unwrap();

        let prom_packet_latency = Histogram::with_opts(
            HistogramOpts::new(
                "cncnet_packet_processing_latency_microseconds",
                "Packet processing latency in microseconds",
            ).buckets(vec![10.0, 50.0, 100.0, 500.0, 1000.0, 5000.0, 10000.0])
        ).unwrap();

        let prom_uptime_seconds = IntGauge::new(
            "cncnet_uptime_seconds",
            "Server uptime in seconds",
        ).unwrap();

        // Register all metrics
        let metrics = vec![
            prometheus::register(Box::new(prom_v3_packets_received.clone())),
            prometheus::register(Box::new(prom_v3_packets_sent.clone())),
            prometheus::register(Box::new(prom_v3_bytes_received.clone())),
            prometheus::register(Box::new(prom_v3_bytes_sent.clone())),
            prometheus::register(Box::new(prom_v3_active_clients.clone())),
            prometheus::register(Box::new(prom_v2_packets_received.clone())),
            prometheus::register(Box::new(prom_v2_packets_sent.clone())),
            prometheus::register(Box::new(prom_v2_active_clients.clone())),
            prometheus::register(Box::new(prom_v2_http_requests.clone())),
            prometheus::register(Box::new(prom_rate_limit_hits.clone())),
            prometheus::register(Box::new(prom_dropped_packets.clone())),
            prometheus::register(Box::new(prom_dropped_connections.clone())),
            prometheus::register(Box::new(prom_packet_latency.clone())),
            prometheus::register(Box::new(prom_uptime_seconds.clone())),
        ];

        // Log any registration errors
        for result in metrics {
            if let Err(e) = result {
                tracing::warn!("Failed to register Prometheus metric: {}", e);
            }
        }

        Self {
            v3_packets_received: AtomicU64::new(0),
            v3_packets_sent: AtomicU64::new(0),
            v3_bytes_received: AtomicU64::new(0),
            v3_bytes_sent: AtomicU64::new(0),
            v3_active_clients: AtomicUsize::new(0),
            v3_ping_requests: AtomicU64::new(0),

            v2_packets_received: AtomicU64::new(0),
            v2_packets_sent: AtomicU64::new(0),
            v2_bytes_received: AtomicU64::new(0),
            v2_bytes_sent: AtomicU64::new(0),
            v2_active_clients: AtomicUsize::new(0),
            v2_http_requests: AtomicU64::new(0),
            v2_ping_requests: AtomicU64::new(0),

            p2p_requests: AtomicU64::new(0),
            p2p_responses: AtomicU64::new(0),

            rate_limit_hits: AtomicU64::new(0),
            dropped_packets: AtomicU64::new(0),
            dropped_connections: AtomicU64::new(0),
            invalid_packets: AtomicU64::new(0),
            memory_usage: AtomicU64::new(0),
            cpu_usage: AtomicU64::new(0),

            packet_processing_time_us: AtomicU64::new(0),
            forwarding_latency_us: AtomicU64::new(0),

            start_time: Instant::now(),

            prom_v3_packets_received,
            prom_v3_packets_sent,
            prom_v3_bytes_received,
            prom_v3_bytes_sent,
            prom_v3_active_clients,
            prom_v2_packets_received,
            prom_v2_packets_sent,
            prom_v2_active_clients,
            prom_v2_http_requests,
            prom_rate_limit_hits,
            prom_dropped_packets,
            prom_dropped_connections,
            prom_packet_latency,
            prom_uptime_seconds,
        }
    }

    pub fn update_prometheus(&self) {
        // Update Prometheus metrics from atomics
        let v3_packets_rx = self.v3_packets_received.load(Ordering::Relaxed);
        let v3_packets_tx = self.v3_packets_sent.load(Ordering::Relaxed);
        let v3_bytes_rx = self.v3_bytes_received.load(Ordering::Relaxed);
        let v3_bytes_tx = self.v3_bytes_sent.load(Ordering::Relaxed);
        let v3_clients = self.v3_active_clients.load(Ordering::Relaxed);

        let v2_packets_rx = self.v2_packets_received.load(Ordering::Relaxed);
        let v2_packets_tx = self.v2_packets_sent.load(Ordering::Relaxed);
        let v2_clients = self.v2_active_clients.load(Ordering::Relaxed);
        let v2_http = self.v2_http_requests.load(Ordering::Relaxed);

        let rate_limits = self.rate_limit_hits.load(Ordering::Relaxed);
        let dropped_pkts = self.dropped_packets.load(Ordering::Relaxed);
        let dropped_conns = self.dropped_connections.load(Ordering::Relaxed);

        // Set counter values (these should be incremental, but we'll use absolute for simplicity)
        self.prom_v3_active_clients.set(v3_clients as i64);
        self.prom_v2_active_clients.set(v2_clients as i64);

        // Update uptime
        let uptime = self.start_time.elapsed().as_secs();
        self.prom_uptime_seconds.set(uptime as i64);

        // Record packet processing latency if available
        let latency = self.packet_processing_time_us.load(Ordering::Relaxed);
        if latency > 0 {
            self.prom_packet_latency.observe(latency as f64);
        }
    }

    pub fn export_prometheus(&self) -> String {
        self.update_prometheus();

        let encoder = TextEncoder::new();
        let metric_families = prometheus::gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer).unwrap();
        String::from_utf8(buffer).unwrap_or_else(|_| String::from("Error encoding metrics"))
    }

    pub fn record_packet_latency(&self, latency_us: u64) {
        self.packet_processing_time_us.store(latency_us, Ordering::Relaxed);
        self.prom_packet_latency.observe(latency_us as f64);
    }

    pub fn get_stats_summary(&self) -> StatsSummary {
        StatsSummary {
            v3_packets_rx: self.v3_packets_received.load(Ordering::Relaxed),
            v3_packets_tx: self.v3_packets_sent.load(Ordering::Relaxed),
            v3_bytes_rx: self.v3_bytes_received.load(Ordering::Relaxed),
            v3_bytes_tx: self.v3_bytes_sent.load(Ordering::Relaxed),
            v3_clients: self.v3_active_clients.load(Ordering::Relaxed),
            v2_packets_rx: self.v2_packets_received.load(Ordering::Relaxed),
            v2_packets_tx: self.v2_packets_sent.load(Ordering::Relaxed),
            v2_clients: self.v2_active_clients.load(Ordering::Relaxed),
            p2p_requests: self.p2p_requests.load(Ordering::Relaxed),
            rate_limit_hits: self.rate_limit_hits.load(Ordering::Relaxed),
            dropped_packets: self.dropped_packets.load(Ordering::Relaxed),
            dropped_connections: self.dropped_connections.load(Ordering::Relaxed),
            uptime_seconds: self.start_time.elapsed().as_secs(),
        }
    }

    pub fn report(&self) {
        let stats = self.get_stats_summary();

        tracing::info!(
            "=== Server Statistics ===\n\
             Uptime: {} seconds\n\
             Tunnel V3: {} clients, {} packets RX, {} packets TX, {} MB RX, {} MB TX\n\
             Tunnel V2: {} clients, {} packets RX, {} packets TX\n\
             P2P: {} requests\n\
             Issues: {} rate limits, {} dropped packets, {} dropped connections",
            stats.uptime_seconds,
            stats.v3_clients,
            stats.v3_packets_rx,
            stats.v3_packets_tx,
            stats.v3_bytes_rx / (1024 * 1024),
            stats.v3_bytes_tx / (1024 * 1024),
            stats.v2_clients,
            stats.v2_packets_rx,
            stats.v2_packets_tx,
            stats.p2p_requests,
            stats.rate_limit_hits,
            stats.dropped_packets,
            stats.dropped_connections
        );
    }

    pub fn reset_counters(&self) {
        // Reset non-cumulative counters (useful for testing)
        self.packet_processing_time_us.store(0, Ordering::Release);
        self.forwarding_latency_us.store(0, Ordering::Release);
    }

    pub fn update_memory_usage(&self) {
        // Attempt to get memory usage
        #[cfg(target_os = "linux")]
        {
            if let Ok(contents) = std::fs::read_to_string("/proc/self/status") {
                for line in contents.lines() {
                    if line.starts_with("VmRSS:") {
                        if let Some(kb_str) = line.split_whitespace().nth(1) {
                            if let Ok(kb) = kb_str.parse::<u64>() {
                                self.memory_usage.store(kb * 1024, Ordering::Relaxed);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct StatsSummary {
    pub v3_packets_rx: u64,
    pub v3_packets_tx: u64,
    pub v3_bytes_rx: u64,
    pub v3_bytes_tx: u64,
    pub v3_clients: usize,
    pub v2_packets_rx: u64,
    pub v2_packets_tx: u64,
    pub v2_clients: usize,
    pub p2p_requests: u64,
    pub rate_limit_hits: u64,
    pub dropped_packets: u64,
    pub dropped_connections: u64,
    pub uptime_seconds: u64,
}

impl StatsSummary {
    pub fn throughput_mbps(&self) -> f64 {
        if self.uptime_seconds == 0 {
            return 0.0;
        }

        let total_bytes = self.v3_bytes_rx + self.v3_bytes_tx +
            (self.v2_packets_rx + self.v2_packets_tx) * 512; // Estimate for V2
        let total_bits = total_bytes * 8;
        (total_bits as f64) / (self.uptime_seconds as f64) / 1_000_000.0
    }

    pub fn packets_per_second(&self) -> f64 {
        if self.uptime_seconds == 0 {
            return 0.0;
        }

        let total_packets = self.v3_packets_rx + self.v3_packets_tx +
            self.v2_packets_rx + self.v2_packets_tx;
        (total_packets as f64) / (self.uptime_seconds as f64)
    }

    pub fn drop_rate_percent(&self) -> f64 {
        let total_packets = self.v3_packets_rx + self.v2_packets_rx;
        if total_packets == 0 {
            return 0.0;
        }

        (self.dropped_packets as f64 / total_packets as f64) * 100.0
    }
}