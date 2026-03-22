use prometheus::{Encoder, Histogram, HistogramOpts, IntCounter, IntGauge, TextEncoder};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

pub struct Metrics {
    pub v3_packets_received: AtomicU64,
    pub v3_packets_sent: AtomicU64,
    pub v3_bytes_received: AtomicU64,
    pub v3_bytes_sent: AtomicU64,
    pub v3_active_clients: AtomicUsize,
    pub v3_ping_requests: AtomicU64,

    pub v2_packets_received: AtomicU64,
    pub v2_packets_sent: AtomicU64,
    pub v2_bytes_received: AtomicU64,
    pub v2_bytes_sent: AtomicU64,
    pub v2_active_clients: AtomicUsize,
    pub v2_http_requests: AtomicU64,
    pub v2_ping_requests: AtomicU64,

    pub p2p_requests: AtomicU64,
    pub p2p_responses: AtomicU64,

    pub rate_limit_hits: AtomicU64,
    pub dropped_packets: AtomicU64,
    pub dropped_connections: AtomicU64,
    pub invalid_packets: AtomicU64,

    start_time: Instant,

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

    _prom_packet_latency: Histogram,
    prom_uptime_seconds: IntGauge,

    last_v3_rx: AtomicU64,
    last_v3_tx: AtomicU64,
    last_v3_bytes_rx: AtomicU64,
    last_v3_bytes_tx: AtomicU64,
    last_v2_rx: AtomicU64,
    last_v2_tx: AtomicU64,
    last_v2_http: AtomicU64,
    last_rate_limits: AtomicU64,
    last_dropped_pkts: AtomicU64,
    last_dropped_conns: AtomicU64,
}

/// Per-worker batch counters that reduce atomic contention on the hot path.
/// Each worker owns one of these mutably; call `flush()` every N packets.
pub struct LocalMetricsBatch {
    v3_packets_rx: u32,
    v3_packets_tx: u32,
    v3_bytes_rx: u64,
    v3_bytes_tx: u64,
    v2_packets_rx: u32,
    v2_packets_tx: u32,
    v2_bytes_rx: u64,
    v2_bytes_tx: u64,
    counter: u16,
}

const FLUSH_INTERVAL: u16 = 64;

impl LocalMetricsBatch {
    pub fn new() -> Self {
        Self {
            v3_packets_rx: 0,
            v3_packets_tx: 0,
            v3_bytes_rx: 0,
            v3_bytes_tx: 0,
            v2_packets_rx: 0,
            v2_packets_tx: 0,
            v2_bytes_rx: 0,
            v2_bytes_tx: 0,
            counter: 0,
        }
    }

    #[inline(always)]
    pub fn record_v3_rx(&mut self, bytes: u64) {
        self.v3_packets_rx += 1;
        self.v3_bytes_rx += bytes;
    }

    #[inline(always)]
    pub fn record_v3_tx(&mut self, bytes: u64) {
        self.v3_packets_tx += 1;
        self.v3_bytes_tx += bytes;
    }

    #[inline(always)]
    pub fn record_v2_rx(&mut self, bytes: u64) {
        self.v2_packets_rx += 1;
        self.v2_bytes_rx += bytes;
    }

    #[inline(always)]
    pub fn record_v2_tx(&mut self, bytes: u64) {
        self.v2_packets_tx += 1;
        self.v2_bytes_tx += bytes;
    }

    #[inline(always)]
    pub fn maybe_flush(&mut self, metrics: &Metrics) {
        self.counter = self.counter.wrapping_add(1);
        if self.counter % FLUSH_INTERVAL == 0 {
            self.flush(metrics);
        }
    }

    pub fn flush(&mut self, metrics: &Metrics) {
        if self.v3_packets_rx > 0 {
            metrics.v3_packets_received.fetch_add(self.v3_packets_rx as u64, Ordering::Relaxed);
            self.v3_packets_rx = 0;
        }
        if self.v3_packets_tx > 0 {
            metrics.v3_packets_sent.fetch_add(self.v3_packets_tx as u64, Ordering::Relaxed);
            self.v3_packets_tx = 0;
        }
        if self.v3_bytes_rx > 0 {
            metrics.v3_bytes_received.fetch_add(self.v3_bytes_rx, Ordering::Relaxed);
            self.v3_bytes_rx = 0;
        }
        if self.v3_bytes_tx > 0 {
            metrics.v3_bytes_sent.fetch_add(self.v3_bytes_tx, Ordering::Relaxed);
            self.v3_bytes_tx = 0;
        }
        if self.v2_packets_rx > 0 {
            metrics.v2_packets_received.fetch_add(self.v2_packets_rx as u64, Ordering::Relaxed);
            self.v2_packets_rx = 0;
        }
        if self.v2_packets_tx > 0 {
            metrics.v2_packets_sent.fetch_add(self.v2_packets_tx as u64, Ordering::Relaxed);
            self.v2_packets_tx = 0;
        }
        if self.v2_bytes_rx > 0 {
            metrics.v2_bytes_received.fetch_add(self.v2_bytes_rx, Ordering::Relaxed);
            self.v2_bytes_rx = 0;
        }
        if self.v2_bytes_tx > 0 {
            metrics.v2_bytes_sent.fetch_add(self.v2_bytes_tx, Ordering::Relaxed);
            self.v2_bytes_tx = 0;
        }
    }
}

impl Metrics {
    pub fn new() -> Self {
        let prom_v3_packets_received = IntCounter::new(
            "cncnet_v3_packets_received_total",
            "Total packets received by Tunnel V3",
        )
        .unwrap();
        let prom_v3_packets_sent = IntCounter::new(
            "cncnet_v3_packets_sent_total",
            "Total packets sent by Tunnel V3",
        )
        .unwrap();
        let prom_v3_bytes_received = IntCounter::new(
            "cncnet_v3_bytes_received_total",
            "Total bytes received by Tunnel V3",
        )
        .unwrap();
        let prom_v3_bytes_sent =
            IntCounter::new("cncnet_v3_bytes_sent_total", "Total bytes sent by Tunnel V3")
                .unwrap();
        let prom_v3_active_clients =
            IntGauge::new("cncnet_v3_active_clients", "Active clients in Tunnel V3").unwrap();

        let prom_v2_packets_received = IntCounter::new(
            "cncnet_v2_packets_received_total",
            "Total packets received by Tunnel V2",
        )
        .unwrap();
        let prom_v2_packets_sent = IntCounter::new(
            "cncnet_v2_packets_sent_total",
            "Total packets sent by Tunnel V2",
        )
        .unwrap();
        let prom_v2_active_clients =
            IntGauge::new("cncnet_v2_active_clients", "Active clients in Tunnel V2").unwrap();
        let prom_v2_http_requests = IntCounter::new(
            "cncnet_v2_http_requests_total",
            "Total HTTP requests to Tunnel V2",
        )
        .unwrap();

        let prom_rate_limit_hits =
            IntCounter::new("cncnet_rate_limit_hits_total", "Total rate limit hits").unwrap();
        let prom_dropped_packets =
            IntCounter::new("cncnet_dropped_packets_total", "Total dropped packets").unwrap();
        let prom_dropped_connections = IntCounter::new(
            "cncnet_dropped_connections_total",
            "Total dropped connections",
        )
        .unwrap();

        let prom_packet_latency = Histogram::with_opts(
            HistogramOpts::new(
                "cncnet_packet_processing_latency_microseconds",
                "Packet processing latency in microseconds",
            )
            .buckets(vec![
                10.0, 50.0, 100.0, 500.0, 1000.0, 5000.0, 10000.0,
            ]),
        )
        .unwrap();

        let prom_uptime_seconds =
            IntGauge::new("cncnet_uptime_seconds", "Server uptime in seconds").unwrap();

        let registrations: Vec<std::result::Result<(), prometheus::Error>> = vec![
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

        for result in registrations {
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
            _prom_packet_latency: prom_packet_latency,
            prom_uptime_seconds,

            last_v3_rx: AtomicU64::new(0),
            last_v3_tx: AtomicU64::new(0),
            last_v3_bytes_rx: AtomicU64::new(0),
            last_v3_bytes_tx: AtomicU64::new(0),
            last_v2_rx: AtomicU64::new(0),
            last_v2_tx: AtomicU64::new(0),
            last_v2_http: AtomicU64::new(0),
            last_rate_limits: AtomicU64::new(0),
            last_dropped_pkts: AtomicU64::new(0),
            last_dropped_conns: AtomicU64::new(0),
        }
    }

    pub fn update_prometheus(&self) {
        let v3_rx = self.v3_packets_received.load(Ordering::Relaxed);
        let v3_tx = self.v3_packets_sent.load(Ordering::Relaxed);
        let v3_bytes_rx = self.v3_bytes_received.load(Ordering::Relaxed);
        let v3_bytes_tx = self.v3_bytes_sent.load(Ordering::Relaxed);
        let v3_clients = self.v3_active_clients.load(Ordering::Relaxed);

        let v2_rx = self.v2_packets_received.load(Ordering::Relaxed);
        let v2_tx = self.v2_packets_sent.load(Ordering::Relaxed);
        let v2_clients = self.v2_active_clients.load(Ordering::Relaxed);
        let v2_http = self.v2_http_requests.load(Ordering::Relaxed);

        let rate_limits = self.rate_limit_hits.load(Ordering::Relaxed);
        let dropped_pkts = self.dropped_packets.load(Ordering::Relaxed);
        let dropped_conns = self.dropped_connections.load(Ordering::Relaxed);

        let prev_v3_rx = self.last_v3_rx.swap(v3_rx, Ordering::Relaxed);
        let prev_v3_tx = self.last_v3_tx.swap(v3_tx, Ordering::Relaxed);
        let prev_v3_bytes_rx = self.last_v3_bytes_rx.swap(v3_bytes_rx, Ordering::Relaxed);
        let prev_v3_bytes_tx = self.last_v3_bytes_tx.swap(v3_bytes_tx, Ordering::Relaxed);
        let prev_v2_rx = self.last_v2_rx.swap(v2_rx, Ordering::Relaxed);
        let prev_v2_tx = self.last_v2_tx.swap(v2_tx, Ordering::Relaxed);
        let prev_v2_http = self.last_v2_http.swap(v2_http, Ordering::Relaxed);
        let prev_rate_limits = self.last_rate_limits.swap(rate_limits, Ordering::Relaxed);
        let prev_dropped_pkts = self.last_dropped_pkts.swap(dropped_pkts, Ordering::Relaxed);
        let prev_dropped_conns = self.last_dropped_conns.swap(dropped_conns, Ordering::Relaxed);

        self.prom_v3_packets_received
            .inc_by(v3_rx.saturating_sub(prev_v3_rx));
        self.prom_v3_packets_sent
            .inc_by(v3_tx.saturating_sub(prev_v3_tx));
        self.prom_v3_bytes_received
            .inc_by(v3_bytes_rx.saturating_sub(prev_v3_bytes_rx));
        self.prom_v3_bytes_sent
            .inc_by(v3_bytes_tx.saturating_sub(prev_v3_bytes_tx));
        self.prom_v3_active_clients.set(v3_clients as i64);

        self.prom_v2_packets_received
            .inc_by(v2_rx.saturating_sub(prev_v2_rx));
        self.prom_v2_packets_sent
            .inc_by(v2_tx.saturating_sub(prev_v2_tx));
        self.prom_v2_active_clients.set(v2_clients as i64);
        self.prom_v2_http_requests
            .inc_by(v2_http.saturating_sub(prev_v2_http));

        self.prom_rate_limit_hits
            .inc_by(rate_limits.saturating_sub(prev_rate_limits));
        self.prom_dropped_packets
            .inc_by(dropped_pkts.saturating_sub(prev_dropped_pkts));
        self.prom_dropped_connections
            .inc_by(dropped_conns.saturating_sub(prev_dropped_conns));

        self.prom_uptime_seconds
            .set(self.start_time.elapsed().as_secs() as i64);
    }

    pub fn export_prometheus(&self) -> String {
        self.update_prometheus();

        let encoder = TextEncoder::new();
        let metric_families = prometheus::gather();
        let mut buffer = Vec::with_capacity(4096);
        encoder.encode(&metric_families, &mut buffer).unwrap();
        unsafe { String::from_utf8_unchecked(buffer) }
    }
}
