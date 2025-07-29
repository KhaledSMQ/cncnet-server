//! Common test utilities and helpers
//!
//! This module provides shared utilities for integration and load tests,
//! including test client implementations, metrics collection, and helper functions.
//!
//! ## Architecture
//!
//! The test utilities are designed to:
//! - Minimize test code duplication
//! - Provide realistic client simulations
//! - Enable comprehensive metrics collection
//! - Support both load and integration testing
//!
//! ## Thread Safety
//!
//! All structures in this module are thread-safe and can be shared
//! across multiple tokio tasks using Arc.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use rand::Rng;
use tokio::net::UdpSocket;
use tokio::sync::{oneshot, Semaphore};
use tokio::time::{sleep, timeout};
use tracing::{debug, info};

/// Test metrics for performance monitoring
///
/// Tracks various metrics during test execution to measure
/// server performance and reliability.
#[derive(Debug, Clone)]
pub struct TestMetrics {
    /// Total packets sent
    pub packets_sent: Arc<AtomicU64>,
    /// Total packets received
    pub packets_received: Arc<AtomicU64>,
    /// Total errors encountered
    pub errors: Arc<AtomicU64>,
    /// Average round-trip time in microseconds
    pub avg_rtt_us: Arc<AtomicU64>,
    /// Max round-trip time in microseconds
    pub max_rtt_us: Arc<AtomicU64>,
    /// Min round-trip time in microseconds
    pub min_rtt_us: Arc<AtomicU64>,
    /// Number of timeouts
    pub timeouts: Arc<AtomicU64>,
}

impl TestMetrics {
    /// Creates new test metrics instance
    pub fn new() -> Self {
        Self {
            packets_sent: Arc::new(AtomicU64::new(0)),
            packets_received: Arc::new(AtomicU64::new(0)),
            errors: Arc::new(AtomicU64::new(0)),
            avg_rtt_us: Arc::new(AtomicU64::new(0)),
            max_rtt_us: Arc::new(AtomicU64::new(0)),
            min_rtt_us: Arc::new(AtomicU64::new(u64::MAX)),
            timeouts: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Updates RTT metrics with a new measurement
    pub fn update_rtt(&self, rtt_us: u64) {
        // Update max RTT
        self.max_rtt_us.fetch_max(rtt_us, Ordering::Relaxed);

        // Update min RTT
        self.min_rtt_us.fetch_min(rtt_us, Ordering::Relaxed);

        // Update average RTT (simple moving average)
        let received = self.packets_received.load(Ordering::Relaxed);
        if received > 0 {
            let current_avg = self.avg_rtt_us.load(Ordering::Relaxed);
            let new_avg = (current_avg * (received - 1) + rtt_us) / received;
            self.avg_rtt_us.store(new_avg, Ordering::Relaxed);
        }
    }

    /// Prints a summary of the metrics
    pub fn print_summary(&self) {
        let sent = self.packets_sent.load(Ordering::Relaxed);
        let received = self.packets_received.load(Ordering::Relaxed);
        let errors = self.errors.load(Ordering::Relaxed);
        let timeouts = self.timeouts.load(Ordering::Relaxed);
        let avg_rtt = self.avg_rtt_us.load(Ordering::Relaxed);
        let max_rtt = self.max_rtt_us.load(Ordering::Relaxed);
        let min_rtt = self.min_rtt_us.load(Ordering::Relaxed);

        let loss_rate = if sent > 0 {
            ((sent - received) as f64 / sent as f64) * 100.0
        } else {
            0.0
        };

        info!("=== Test Metrics Summary ===");
        info!("Packets sent: {}", sent);
        info!("Packets received: {}", received);
        info!("Packet loss: {:.2}%", loss_rate);
        info!("Errors: {}", errors);
        info!("Timeouts: {}", timeouts);
        info!(
            "RTT - Avg: {}μs, Min: {}μs, Max: {}μs",
            avg_rtt,
            if min_rtt == u64::MAX { 0 } else { min_rtt },
            max_rtt
        );
        info!("==========================");
    }
}

impl Default for TestMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Simulated test client for tunnel protocols
///
/// Provides a realistic client implementation for testing
/// tunnel server functionality.
#[derive(Clone)]
pub struct TestClient {
    /// Client ID
    pub id: u32,
    /// UDP socket
    pub socket: Arc<UdpSocket>,
    /// Server address
    pub server_addr: SocketAddr,
    /// Test metrics
    pub metrics: TestMetrics,
    /// Client IP (for simulating different IPs)
    pub simulated_ip: IpAddr,
}

impl TestClient {
    /// Creates a new test client
    pub async fn new(id: u32, server_addr: SocketAddr) -> Result<Self> {
        // Bind to random port
        let socket = UdpSocket::bind("0.0.0.0:0").await?;

        // Generate simulated IP for this client
        let mut rng = rand::thread_rng();
        let simulated_ip = IpAddr::V4(Ipv4Addr::new(
            10,
            rng.gen_range(0..255),
            rng.gen_range(0..255),
            rng.gen_range(1..255),
        ));

        Ok(Self {
            id,
            socket: Arc::new(socket),
            server_addr,
            metrics: TestMetrics::new(),
            simulated_ip,
        })
    }

    /// Sends a ping packet and measures RTT
    pub async fn ping(&self) -> Result<Duration> {
        let mut packet = vec![0u8; 50];

        // Fill with ping pattern (V3 uses little-endian)
        packet[0..4].copy_from_slice(&0u32.to_le_bytes());
        packet[4..8].copy_from_slice(&0u32.to_le_bytes());

        let start = Instant::now();

        // Send ping
        self.socket.send_to(&packet, self.server_addr).await?;
        self.metrics.packets_sent.fetch_add(1, Ordering::Relaxed);

        // Wait for response
        let mut recv_buf = vec![0u8; 64];
        match timeout(Duration::from_secs(1), self.socket.recv_from(&mut recv_buf)).await {
            Ok(Ok((size, _))) => {
                if size >= 12 {
                    let rtt = start.elapsed();
                    self.metrics
                        .packets_received
                        .fetch_add(1, Ordering::Relaxed);
                    self.metrics.update_rtt(rtt.as_micros() as u64);
                    Ok(rtt)
                } else {
                    self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                    Err(anyhow::anyhow!("Invalid response size"))
                }
            }
            Ok(Err(e)) => {
                self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                Err(e.into())
            }
            Err(_) => {
                self.metrics.timeouts.fetch_add(1, Ordering::Relaxed);
                Err(anyhow::anyhow!("Ping timeout"))
            }
        }
    }

    /// Sends a tunnel packet to another client
    pub async fn send_tunnel_packet(&self, receiver_id: u32, data: &[u8]) -> Result<()> {
        let mut packet = Vec::with_capacity(8 + data.len());

        // Add header (sender_id, receiver_id) - V3 uses little-endian
        packet.extend_from_slice(&self.id.to_le_bytes());
        packet.extend_from_slice(&receiver_id.to_le_bytes());
        packet.extend_from_slice(data);

        self.socket.send_to(&packet, self.server_addr).await?;
        self.metrics.packets_sent.fetch_add(1, Ordering::Relaxed);

        Ok(())
    }

    /// Receives a tunnel packet
    pub async fn recv_tunnel_packet(&self, timeout_duration: Duration) -> Result<(u32, Vec<u8>)> {
        let mut recv_buf = vec![0u8; 1024];

        match timeout(timeout_duration, self.socket.recv_from(&mut recv_buf)).await {
            Ok(Ok((size, _))) => {
                if size >= 8 {
                    let sender_id = u32::from_le_bytes([
                        recv_buf[0],
                        recv_buf[1],
                        recv_buf[2],
                        recv_buf[3],
                    ]);
                    let data = recv_buf[8..size].to_vec();
                    self.metrics
                        .packets_received
                        .fetch_add(1, Ordering::Relaxed);
                    Ok((sender_id, data))
                } else {
                    self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                    Err(anyhow::anyhow!("Invalid packet size"))
                }
            }
            Ok(Err(e)) => {
                self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                Err(e.into())
            }
            Err(_) => {
                self.metrics.timeouts.fetch_add(1, Ordering::Relaxed);
                Err(anyhow::anyhow!("Receive timeout"))
            }
        }
    }
}

/// Starts a test server instance
pub async fn start_test_server(
    port: u16,
    max_clients: usize,
) -> Result<(oneshot::Sender<()>, tokio::task::JoinHandle<Result<()>>)> {
    use cncnet_server::net::tunnel_v3::TunnelV3;

    let tunnel = TunnelV3::new(
        port,
        max_clients,
        "Test Server".to_string(),
        true, // no master announce
        String::new(),
        "test123".to_string(),
        String::new(),
        8, // ip limit
    );

    tunnel.start().await
}

/// Creates multiple test clients
pub async fn create_test_clients(
    count: usize,
    server_addr: SocketAddr,
) -> Result<Vec<TestClient>> {
    let mut clients = Vec::with_capacity(count);

    for i in 0..count {
        let client = TestClient::new(i as u32 + 1, server_addr).await?;
        clients.push(client);
    }

    Ok(clients)
}

/// Runs a concurrent load test with the given clients
pub async fn run_concurrent_load<F, Fut>(
    clients: Vec<TestClient>,
    duration: Duration,
    concurrency: usize,
    operation: F,
) -> Result<()>
where
    F: Fn(TestClient) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send,
{
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let operation = Arc::new(operation);
    let end_time = Instant::now() + duration;

    let mut handles = Vec::new();

    for client in clients {
        let semaphore = semaphore.clone();
        let operation = operation.clone();

        let handle = tokio::spawn(async move {
            while Instant::now() < end_time {
                let _permit = semaphore.acquire().await.unwrap();

                if let Err(e) = operation(client.clone()).await {
                    debug!("Operation error: {}", e);
                    client.metrics.errors.fetch_add(1, Ordering::Relaxed);
                }

                // Small delay to prevent overwhelming
                sleep(Duration::from_millis(10)).await;
            }
        });

        handles.push(handle);
    }

    // Wait for all tasks to complete
    for handle in handles {
        handle.await?;
    }

    Ok(())
}

/// Generates random game data for testing
pub fn generate_game_data(size: usize) -> Vec<u8> {
    let mut rng = rand::thread_rng();
    let mut data = vec![0u8; size];
    for byte in &mut data {
        *byte = rng.gen();
    }
    data
}

/// Waits for server to be ready by sending pings
pub async fn wait_for_server(addr: SocketAddr, timeout_secs: u64) -> Result<()> {
    let start = Instant::now();
    let timeout_duration = Duration::from_secs(timeout_secs);

    info!("Waiting for server at {} to be ready...", addr);

    loop {
        if start.elapsed() > timeout_duration {
            return Err(anyhow::anyhow!("Server startup timeout"));
        }

        // Try to create a test client and ping
        match TestClient::new(0, addr).await {
            Ok(client) => match client.ping().await {
                Ok(_) => {
                    info!("Server is ready!");
                    return Ok(());
                }
                Err(_) => {
                    // Server not ready yet
                }
            },
            Err(_) => {
                // Socket creation failed
            }
        }

        sleep(Duration::from_millis(100)).await;
    }
}

/// Test harness for running a complete test scenario
pub struct TestHarness {
    pub server_handle: tokio::task::JoinHandle<Result<()>>,
    pub shutdown_tx: oneshot::Sender<()>,
    pub server_addr: SocketAddr,
}

impl TestHarness {
    /// Creates a new test harness with a running server
    pub async fn new(port: u16, max_clients: usize) -> Result<Self> {
        let (shutdown_tx, handle) = start_test_server(port, max_clients).await?;
        let server_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);

        // Wait for server to be ready
        wait_for_server(server_addr, 5).await?;

        Ok(Self {
            server_handle: handle,
            shutdown_tx,
            server_addr,
        })
    }

    /// Shuts down the test harness
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.shutdown_tx.send(());
        self.server_handle.await??;
        Ok(())
    }
}

/// Assertion helpers for test validation

/// Asserts that packet loss is below threshold
pub fn assert_packet_loss_below(metrics: &TestMetrics, threshold_percent: f64) {
    let sent = metrics.packets_sent.load(Ordering::Relaxed);
    let received = metrics.packets_received.load(Ordering::Relaxed);

    if sent > 0 {
        let loss_rate = ((sent - received) as f64 / sent as f64) * 100.0;
        assert!(
            loss_rate < threshold_percent,
            "Packet loss {:.2}% exceeds threshold {:.2}%",
            loss_rate,
            threshold_percent
        );
    }
}

/// Asserts that average RTT is below threshold
pub fn assert_avg_rtt_below(metrics: &TestMetrics, threshold_ms: u64) {
    let avg_rtt_us = metrics.avg_rtt_us.load(Ordering::Relaxed);
    let avg_rtt_ms = avg_rtt_us / 1000;

    assert!(
        avg_rtt_ms < threshold_ms,
        "Average RTT {}ms exceeds threshold {}ms",
        avg_rtt_ms,
        threshold_ms
    );
}

/// Asserts that error rate is below threshold
pub fn assert_error_rate_below(metrics: &TestMetrics, threshold_percent: f64) {
    let sent = metrics.packets_sent.load(Ordering::Relaxed);
    let errors = metrics.errors.load(Ordering::Relaxed);

    if sent > 0 {
        let error_rate = (errors as f64 / sent as f64) * 100.0;
        assert!(
            error_rate < threshold_percent,
            "Error rate {:.2}% exceeds threshold {:.2}%",
            error_rate,
            threshold_percent
        );
    }
}

// Compile-time assertions to ensure TestClient is Send + Sync
const _: () = {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    fn assert_all() {
        assert_send::<TestClient>();
        assert_sync::<TestClient>();
        assert_send::<TestMetrics>();
        assert_sync::<TestMetrics>();
    }
};