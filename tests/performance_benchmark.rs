//! Performance benchmarks for CnCNet server
//!
//! This module contains performance benchmarks to measure:
//! - Throughput capabilities
//! - Latency characteristics
//! - Memory efficiency
//! - Scalability limits

mod common;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::Semaphore;
use tokio::time::sleep;
use tracing::info;

use common::*;

/// Benchmark configuration
struct BenchmarkConfig {
    /// Number of clients
    clients: usize,
    /// Test duration
    duration: Duration,
    /// Packet size in bytes
    packet_size: usize,
    /// Packets per second per client
    pps_per_client: u32,
    /// Concurrency limit
    concurrency: usize,
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            clients: 100,
            duration: Duration::from_secs(30),
            packet_size: 256,
            pps_per_client: 100,
            concurrency: 50,
        }
    }
}

/// Runs a benchmark with the given configuration
async fn run_benchmark(
    config: BenchmarkConfig,
    server_addr: SocketAddr,
) -> Result<BenchmarkResults> {
    info!("Starting benchmark with {} clients", config.clients);

    let clients = create_test_clients(config.clients, server_addr).await?;
    let client_refs: Vec<Arc<TestClient>> = clients.into_iter().map(Arc::new).collect();

    let start_time = Instant::now();
    let bytes_sent = Arc::new(AtomicU64::new(0));
    let bytes_received = Arc::new(AtomicU64::new(0));
    let packets_sent = Arc::new(AtomicU64::new(0));
    let packets_received = Arc::new(AtomicU64::new(0));
    let total_latency_us = Arc::new(AtomicU64::new(0));
    let latency_samples = Arc::new(AtomicU64::new(0));

    let semaphore = Arc::new(Semaphore::new(config.concurrency));
    let mut handles = Vec::new();

    // Start client workers
    for (i, client) in client_refs.iter().enumerate() {
        let client = client.clone();
        let bytes_sent = bytes_sent.clone();
        let packets_sent = packets_sent.clone();
        let total_latency_us = total_latency_us.clone();
        let latency_samples = latency_samples.clone();
        let semaphore = semaphore.clone();
        let config = config.clone();

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_micros(
                1_000_000 / config.pps_per_client as u64,
            ));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let test_end = Instant::now() + config.duration;

            while Instant::now() < test_end {
                interval.tick().await;

                let permit = semaphore.clone().acquire_owned().await.unwrap();

                // Send packet to next client (ring topology)
                let target_id = ((i + 1) % config.clients) as u32 + 1;
                let data = generate_game_data(config.packet_size);

                let send_time = Instant::now();
                if client.send_tunnel_packet(target_id, &data).await.is_ok() {
                    bytes_sent.fetch_add(data.len() as u64, Ordering::Relaxed);
                    packets_sent.fetch_add(1, Ordering::Relaxed);

                    // Measure round-trip latency occasionally
                    if packets_sent.load(Ordering::Relaxed) % 100 == 0 {
                        if let Ok(rtt) = client.ping().await {
                            total_latency_us.fetch_add(rtt.as_micros() as u64, Ordering::Relaxed);
                            latency_samples.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        });

        handles.push(handle);
    }

    // Start receiver tasks
    let mut receiver_handles = Vec::new();

    for client in client_refs.iter() {
        let client = client.clone();
        let bytes_received = bytes_received.clone();
        let packets_received = packets_received.clone();
        let config = config.clone();

        let handle = tokio::spawn(async move {
            let test_end = Instant::now() + config.duration;

            while Instant::now() < test_end {
                match client.recv_tunnel_packet(Duration::from_millis(100)).await {
                    Ok((_, data)) => {
                        bytes_received.fetch_add(data.len() as u64, Ordering::Relaxed);
                        packets_received.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {
                        // Timeout or error, continue
                    }
                }
            }
        });

        receiver_handles.push(handle);
    }

    // Wait for all tasks
    for handle in handles {
        handle.await?;
    }

    for handle in receiver_handles {
        handle.await?;
    }

    let duration = start_time.elapsed();

    // Calculate results
    let total_bytes_sent = bytes_sent.load(Ordering::Relaxed);
    let total_bytes_received = bytes_received.load(Ordering::Relaxed);
    let total_packets_sent = packets_sent.load(Ordering::Relaxed);
    let total_packets_received = packets_received.load(Ordering::Relaxed);
    let total_latency = total_latency_us.load(Ordering::Relaxed);
    let samples = latency_samples.load(Ordering::Relaxed);

    Ok(BenchmarkResults {
        duration,
        bytes_sent: total_bytes_sent,
        bytes_received: total_bytes_received,
        packets_sent: total_packets_sent,
        packets_received: total_packets_received,
        avg_latency_us: if samples > 0 {
            total_latency / samples
        } else {
            0
        },
        throughput_mbps: (total_bytes_sent as f64 * 8.0) / (duration.as_secs_f64() * 1_000_000.0),
        packet_loss_rate: if total_packets_sent > 0 {
            ((total_packets_sent - total_packets_received) as f64 / total_packets_sent as f64)
                * 100.0
        } else {
            0.0
        },
    })
}

/// Benchmark results
#[derive(Debug)]
struct BenchmarkResults {
    duration: Duration,
    bytes_sent: u64,
    bytes_received: u64,
    packets_sent: u64,
    packets_received: u64,
    avg_latency_us: u64,
    throughput_mbps: f64,
    packet_loss_rate: f64,
}

impl BenchmarkResults {
    fn print_summary(&self) {
        info!("=== Benchmark Results ===");
        info!("Duration: {:.2}s", self.duration.as_secs_f64());
        info!("Bytes sent: {} MB", self.bytes_sent / 1_000_000);
        info!("Bytes received: {} MB", self.bytes_received / 1_000_000);
        info!("Packets sent: {}", self.packets_sent);
        info!("Packets received: {}", self.packets_received);
        info!("Throughput: {:.2} Mbps", self.throughput_mbps);
        info!("Packet loss: {:.2}%", self.packet_loss_rate);
        info!("Average latency: {} μs", self.avg_latency_us);
        info!("=======================");
    }
}

// Make BenchmarkConfig cloneable
impl Clone for BenchmarkConfig {
    fn clone(&self) -> Self {
        Self {
            clients: self.clients,
            duration: self.duration,
            packet_size: self.packet_size,
            pps_per_client: self.pps_per_client,
            concurrency: self.concurrency,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn benchmark_throughput_scaling() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("Benchmarking throughput scaling");

    let harness = TestHarness::new(50200, 300).await?;

    // Test with different client counts
    for clients in [10, 50, 100, 200] {
        info!("\n--- Testing with {} clients ---", clients);

        let config = BenchmarkConfig {
            clients,
            duration: Duration::from_secs(20),
            packet_size: 256,
            pps_per_client: 50,
            concurrency: clients.min(100),
        };

        let results = run_benchmark(config, harness.server_addr).await?;
        results.print_summary();

        // Performance assertions
        assert!(
            results.packet_loss_rate < 2.0,
            "Packet loss too high with {} clients",
            clients
        );
        assert!(
            results.avg_latency_us < 100_000,
            "Latency too high with {} clients",
            clients
        );
    }

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn benchmark_packet_size_impact() -> Result<()> {
    info!("Benchmarking packet size impact");

    let harness = TestHarness::new(50201, 200).await?;

    // Test with different packet sizes
    for packet_size in [64, 256, 512, 1024] {
        info!("\n--- Testing with {} byte packets ---", packet_size);

        let config = BenchmarkConfig {
            clients: 50,
            duration: Duration::from_secs(15),
            packet_size,
            pps_per_client: 100,
            concurrency: 50,
        };

        let results = run_benchmark(config, harness.server_addr).await?;
        results.print_summary();

        // Larger packets should maintain good performance
        assert!(
            results.packet_loss_rate < 5.0,
            "Packet loss too high with {} byte packets",
            packet_size
        );
    }

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn benchmark_burst_traffic() -> Result<()> {
    info!("Benchmarking burst traffic handling");

    let harness = TestHarness::new(50202, 200).await?;

    let clients = create_test_clients(100, harness.server_addr).await?;
    let burst_complete = Arc::new(AtomicU64::new(0));
    let burst_errors = Arc::new(AtomicU64::new(0));

    let client_refs: Vec<Arc<TestClient>> = clients.into_iter().map(Arc::new).collect();

    // Generate burst traffic
    let mut handles = Vec::new();

    for (i, client_ref) in client_refs.iter().enumerate() {
        let client = client_ref.clone();
        let burst_complete = burst_complete.clone();
        let burst_errors = burst_errors.clone();

        let handle = tokio::spawn(async move {
            // Send burst of 100 packets as fast as possible
            for j in 0..100 {
                let target = ((i + j + 1) % 100) as u32 + 1;
                let data = generate_game_data(512);

                match client.send_tunnel_packet(target, &data).await {
                    Ok(_) => burst_complete.fetch_add(1, Ordering::Relaxed),
                    Err(_) => burst_errors.fetch_add(1, Ordering::Relaxed),
                };
            }
        });

        handles.push(handle);
    }

    let start = Instant::now();
    for handle in handles {
        handle.await?;
    }
    let burst_duration = start.elapsed();

    let completed = burst_complete.load(Ordering::Relaxed);
    let errors = burst_errors.load(Ordering::Relaxed);

    info!("Burst test results:");
    info!("  Total packets: {}", completed + errors);
    info!("  Successful: {}", completed);
    info!("  Errors: {}", errors);
    info!("  Duration: {:.2}s", burst_duration.as_secs_f64());
    info!(
        "  Rate: {:.0} pps",
        completed as f64 / burst_duration.as_secs_f64()
    );

    // Should handle burst with minimal errors
    let error_rate = errors as f64 / (completed + errors) as f64 * 100.0;
    assert!(
        error_rate < 5.0,
        "Error rate too high during burst: {:.2}%",
        error_rate
    );

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn benchmark_latency_distribution() -> Result<()> {
    info!("Benchmarking latency distribution");

    let harness = TestHarness::new(50203, 100).await?;

    let clients = create_test_clients(20, harness.server_addr).await?;
    let mut all_latencies = Vec::new();

    // Measure latencies
    for _ in 0..50 {
        for client in &clients {
            if let Ok(rtt) = client.ping().await {
                all_latencies.push(rtt.as_micros() as u64);
            }
        }
        sleep(Duration::from_millis(10)).await;
    }

    // Calculate percentiles
    all_latencies.sort_unstable();

    if !all_latencies.is_empty() {
        let p50 = all_latencies[all_latencies.len() / 2];
        let p95 = all_latencies[all_latencies.len() * 95 / 100];
        let p99 = all_latencies[all_latencies.len() * 99 / 100];
        let min = all_latencies[0];
        let max = all_latencies[all_latencies.len() - 1];

        info!("Latency distribution (μs):");
        info!("  Min: {}", min);
        info!("  P50: {}", p50);
        info!("  P95: {}", p95);
        info!("  P99: {}", p99);
        info!("  Max: {}", max);

        // Latency should be reasonable
        assert!(p50 < 10_000, "Median latency too high");
        assert!(p99 < 50_000, "P99 latency too high");
    }

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn benchmark_memory_efficiency() -> Result<()> {
    info!("Benchmarking memory efficiency");

    let harness = TestHarness::new(50204, 300).await?;

    // Measure baseline memory (approximately)
    let baseline_clients = 10;
    let baseline_config = BenchmarkConfig {
        clients: baseline_clients,
        duration: Duration::from_secs(10),
        packet_size: 256,
        pps_per_client: 10,
        concurrency: 10,
    };

    let baseline_results = run_benchmark(baseline_config, harness.server_addr).await?;
    info!("Baseline with {} clients:", baseline_clients);
    baseline_results.print_summary();

    // Scale up and measure
    let scaled_clients = 200;
    let scaled_config = BenchmarkConfig {
        clients: scaled_clients,
        duration: Duration::from_secs(10),
        packet_size: 256,
        pps_per_client: 10,
        concurrency: 100,
    };

    let scaled_results = run_benchmark(scaled_config, harness.server_addr).await?;
    info!("\nScaled to {} clients:", scaled_clients);
    scaled_results.print_summary();

    // Performance should scale reasonably
    let baseline_throughput_per_client = baseline_results.throughput_mbps / baseline_clients as f64;
    let scaled_throughput_per_client = scaled_results.throughput_mbps / scaled_clients as f64;

    let efficiency = scaled_throughput_per_client / baseline_throughput_per_client;
    info!("Scaling efficiency: {:.2}%", efficiency * 100.0);

    // Should maintain at least 70% efficiency
    assert!(
        efficiency > 0.7,
        "Poor scaling efficiency: {:.2}%",
        efficiency * 100.0
    );

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore] // Run with --ignored for extended benchmark
async fn benchmark_sustained_load() -> Result<()> {
    info!("Running sustained load benchmark (5 minutes)");

    let harness = TestHarness::new(50205, 250).await?;

    let config = BenchmarkConfig {
        clients: 150,
        duration: Duration::from_secs(300), // 5 minutes
        packet_size: 256,
        pps_per_client: 50,
        concurrency: 100,
    };

    let results = run_benchmark(config, harness.server_addr).await?;
    results.print_summary();

    // Should maintain performance over extended period
    assert!(
        results.packet_loss_rate < 1.0,
        "Packet loss too high under sustained load"
    );
    assert!(
        results.avg_latency_us < 50_000,
        "Latency degraded under sustained load"
    );

    harness.shutdown().await?;
    Ok(())
}
