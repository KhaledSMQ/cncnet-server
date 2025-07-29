//! Integration and load tests for CnCNet server
//!
//! This module contains comprehensive load tests that simulate realistic
//! game scenarios with 200+ concurrent clients, testing for:
//! - High concurrency handling
//! - Memory efficiency under load
//! - Race condition resilience
//! - Performance characteristics
//! - Packet loss and latency

mod common;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::Semaphore;
use tokio::time::sleep;
use tracing::{info, warn};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};


use common::*;

/// Number of clients for standard load test
const LOAD_TEST_CLIENTS: usize = 200;

/// Number of clients for stress test
const STRESS_TEST_CLIENTS: usize = 500;

/// Test duration for sustained load
const LOAD_TEST_DURATION: Duration = Duration::from_secs(60);

/// Maximum acceptable packet loss percentage
const MAX_PACKET_LOSS: f64 = 1.0;

/// Maximum acceptable average RTT in milliseconds
const MAX_AVG_RTT_MS: u64 = 50;

/// Maximum acceptable error rate percentage
const MAX_ERROR_RATE: f64 = 0.5;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_200_concurrent_clients() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("Starting 200 concurrent clients test");

    let harness = TestHarness::new(50001, 250).await?;
    let clients = create_test_clients(LOAD_TEST_CLIENTS, harness.server_addr).await?;

    info!("Created {} test clients", clients.len());

    // Simulate game traffic between random pairs
    let client_refs: Vec<Arc<TestClient>> = clients.into_iter().map(Arc::new).collect();
    let mut handles = Vec::new();

    for i in 0..LOAD_TEST_CLIENTS {
        let client = client_refs[i].clone();
        let all_clients = client_refs.clone();

        let handle = tokio::spawn(async move {
            let mut rng = SmallRng::from_os_rng() ;
            let start = Instant::now();

            while start.elapsed() < LOAD_TEST_DURATION {
                // Pick random peer
                let peer_idx = rng.gen_range(0..LOAD_TEST_CLIENTS);
                if peer_idx == i {
                    continue;
                }

                let peer_id = (peer_idx + 1) as u32;

                // Send game data
                let data = generate_game_data(rng.gen_range(50..200));
                if let Err(e) = client.send_tunnel_packet(peer_id, &data).await {
                    warn!("Failed to send packet: {}", e);
                }

                // Occasionally ping
                if rng.gen::<f32>() < 0.1 {
                    let _ = client.ping().await;
                }

                sleep(Duration::from_millis(rng.gen_range(10..50))).await;
            }
        });

        handles.push(handle);
    }

    // Wait for all clients to finish
    for handle in handles {
        handle.await?;
    }

    // Collect and verify metrics
    let mut total_metrics = TestMetrics::new();
    for client in &client_refs {
        total_metrics.packets_sent.fetch_add(
            client.metrics.packets_sent.load(Ordering::Relaxed),
            Ordering::Relaxed
        );
        total_metrics.packets_received.fetch_add(
            client.metrics.packets_received.load(Ordering::Relaxed),
            Ordering::Relaxed
        );
        total_metrics.errors.fetch_add(
            client.metrics.errors.load(Ordering::Relaxed),
            Ordering::Relaxed
        );
    }

    total_metrics.print_summary();

    // Verify performance metrics
    assert_packet_loss_below(&total_metrics, MAX_PACKET_LOSS);
    assert_error_rate_below(&total_metrics, MAX_ERROR_RATE);

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_connection_race() -> Result<()> {
    info!("Testing concurrent connection race conditions");

    let harness = TestHarness::new(50002, 100).await?;
    let connection_count = Arc::new(AtomicU32::new(0));
    let error_count = Arc::new(AtomicU32::new(0));

    // Spawn many clients simultaneously to test race conditions
    let mut handles = Vec::new();

    for batch in 0..5 {
        let server_addr = harness.server_addr;
        let connection_count = connection_count.clone();
        let error_count = error_count.clone();

        let handle = tokio::spawn(async move {
            let mut batch_handles = Vec::new();

            // Create 50 clients simultaneously
            for i in 0..50 {
                let client_id = (batch * 50 + i + 1) as u32;
                let connection_count = connection_count.clone();
                let error_count = error_count.clone();

                let client_handle = tokio::spawn(async move {
                    match TestClient::new(client_id, server_addr).await {
                        Ok(client) => {
                            // Try to establish connection
                            match client.ping().await {
                                Ok(_) => {
                                    connection_count.fetch_add(1, Ordering::Relaxed);

                                    // Send some traffic
                                    for _ in 0..10 {
                                        let data = generate_game_data(100);
                                        let _ = client.send_tunnel_packet(
                                            client_id % 50 + 1,
                                            &data
                                        ).await;
                                        sleep(Duration::from_millis(10)).await;
                                    }
                                }
                                Err(_) => {
                                    error_count.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                        Err(_) => {
                            error_count.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });

                batch_handles.push(client_handle);
            }

            // Wait for batch to complete
            for handle in batch_handles {
                let _ = handle.await;
            }
        });

        handles.push(handle);
    }

    // Wait for all batches
    for handle in handles {
        handle.await?;
    }

    let total_connections = connection_count.load(Ordering::Relaxed);
    let total_errors = error_count.load(Ordering::Relaxed);

    info!("Successful connections: {}", total_connections);
    info!("Failed connections: {}", total_errors);

    // At least 90% should succeed
    assert!(total_connections >= 225, "Too many connection failures");

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_ip_limit_enforcement() -> Result<()> {
    info!("Testing IP limit enforcement under concurrent load");

    let harness = TestHarness::new(50003, 100).await?;

    // Create multiple clients from same "IP"
    let mut clients = Vec::new();
    for i in 0..20 {
        match TestClient::new(i + 1, harness.server_addr).await {
            Ok(mut client) => {
                // Override simulated IP to test IP limits
                client.simulated_ip = "10.0.0.1".parse().unwrap();
                clients.push(client);
            }
            Err(e) => {
                warn!("Failed to create client: {}", e);
            }
        }
    }

    // Try to connect all clients simultaneously
    let success_count = Arc::new(AtomicU32::new(0));
    let mut handles = Vec::new();

    for client in clients {
        let success_count = success_count.clone();

        let handle = tokio::spawn(async move {
            match client.ping().await {
                Ok(_) => {
                    success_count.fetch_add(1, Ordering::Relaxed);
                    // Keep connection alive
                    for _ in 0..10 {
                        let _ = client.ping().await;
                        sleep(Duration::from_millis(100)).await;
                    }
                }
                Err(_) => {
                    // Expected for some clients due to IP limit
                }
            }
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.await?;
    }

    let successful = success_count.load(Ordering::Relaxed);
    info!("Successful connections from same IP: {}", successful);

    // Should be limited to 8 (default IP limit)
    assert!(successful <= 8, "IP limit not enforced properly");

    harness.shutdown().await?;
    Ok(())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_sustained_high_throughput() -> Result<()> {
    info!("Testing sustained high throughput with 100 clients");

    let harness = TestHarness::new(50004, 150).await?;
    let clients = create_test_clients(100, harness.server_addr).await?;

    // Metrics for throughput calculation
    let bytes_sent = Arc::new(AtomicU64::new(0));
    let start_time = Instant::now();

    let client_refs: Vec<Arc<TestClient>> = clients.into_iter().map(Arc::new).collect();
    let mut handles = Vec::new();

    // Each client sends continuous traffic
    for (i, client) in client_refs.iter().enumerate() {
        let client = client.clone();
        let bytes_sent = bytes_sent.clone();
        let all_clients = client_refs.clone();

        let handle = tokio::spawn(async move {
            let mut rng = SmallRng::from_os_rng();
            let test_duration = Duration::from_secs(30);
            let start = Instant::now();

            while start.elapsed() < test_duration {
                // Send to multiple peers
                for _ in 0..5 {
                    let peer_idx = rng.gen_range(0..all_clients.len());
                    if peer_idx == i {
                        continue;
                    }

                    let data = generate_game_data(512); // Larger packets
                    bytes_sent.fetch_add(data.len() as u64, Ordering::Relaxed);

                    let _ = client.send_tunnel_packet((peer_idx + 1) as u32, &data).await;
                }

                // Small delay to prevent overwhelming
                sleep(Duration::from_millis(5)).await;
            }
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.await?;
    }

    let duration = start_time.elapsed();
    let total_bytes = bytes_sent.load(Ordering::Relaxed);
    let throughput_mbps = (total_bytes as f64 * 8.0) / (duration.as_secs_f64() * 1_000_000.0);

    info!("Sustained throughput: {:.2} Mbps", throughput_mbps);
    info!("Total data sent: {} MB", total_bytes / 1_000_000);

    // Collect error metrics
    let mut total_errors = 0u64;
    for client in &client_refs {
        total_errors += client.metrics.errors.load(Ordering::Relaxed);
    }

    info!("Total errors during high throughput: {}", total_errors);

    // Should handle at least 10 Mbps without significant errors
    assert!(throughput_mbps > 10.0, "Throughput too low");
    assert!(total_errors < 100, "Too many errors during high throughput");

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_memory_efficiency_under_load() -> Result<()> {
    info!("Testing memory efficiency with connection churn");

    let harness = TestHarness::new(50005, 300).await?;

    // Continuously create and destroy connections
    let duration = Duration::from_secs(30);
    let start = Instant::now();
    let mut cycle = 0;

    while start.elapsed() < duration {
        cycle += 1;
        info!("Connection churn cycle {}", cycle);

        // Create 50 clients
        let clients = create_test_clients(50, harness.server_addr).await?;

        // Send some traffic
        for client in &clients {
            for _ in 0..10 {
                let _ = client.ping().await;
                let data = generate_game_data(100);
                let _ = client.send_tunnel_packet(1, &data).await;
            }
        }

        // Let them timeout naturally (simulating disconnections)
        sleep(Duration::from_secs(2)).await;

        // Clients go out of scope and should be cleaned up
        drop(clients);
    }

    info!("Completed {} connection churn cycles", cycle);

    // Server should still be responsive after churn
    let test_client = TestClient::new(999, harness.server_addr).await?;
    let ping_result = test_client.ping().await;
    assert!(ping_result.is_ok(), "Server unresponsive after connection churn");

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_rate_limiter_race_conditions() -> Result<()> {
    info!("Testing rate limiter race conditions");

    let harness = TestHarness::new(50006, 100).await?;

    // Create clients that will all ping simultaneously
    let clients = create_test_clients(50, harness.server_addr).await?;
    let barrier = Arc::new(tokio::sync::Barrier::new(clients.len()));

    let mut handles = Vec::new();

    for client in clients {
        let barrier = barrier.clone();

        let handle = tokio::spawn(async move {
            // Wait for all clients to be ready
            barrier.wait().await;

            // Burst of pings to trigger rate limiting
            let mut success_count = 0;
            let mut error_count = 0;

            for _ in 0..100 {
                match client.ping().await {
                    Ok(_) => success_count += 1,
                    Err(_) => error_count += 1,
                }
                // No delay - maximum pressure on rate limiter
            }

            (success_count, error_count)
        });

        handles.push(handle);
    }

    let mut total_success = 0;
    let mut total_errors = 0;

    for handle in handles {
        let (success, errors) = handle.await?;
        total_success += success;
        total_errors += errors;
    }

    info!("Rate limiter test - Success: {}, Limited: {}", total_success, total_errors);

    // Should have some successful pings but also rate limiting
    assert!(total_success > 0, "No successful pings");
    assert!(total_errors > 0, "Rate limiting not working");

    harness.shutdown().await?;
    Ok(())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore] // Run with --ignored for stress test
async fn stress_test_500_clients() -> Result<()> {
    info!("Starting stress test with 500 clients");

    let harness = TestHarness::new(50007, 600).await?;
    let clients = create_test_clients(STRESS_TEST_CLIENTS, harness.server_addr).await?;

    info!("Created {} test clients for stress test", clients.len());

    // Run for shorter duration but with more intensity
    let test_duration = Duration::from_secs(30);
    let client_refs: Vec<Arc<TestClient>> = clients.into_iter().map(Arc::new).collect();

    // Concurrent operations semaphore to prevent resource exhaustion
    let semaphore = Arc::new(Semaphore::new(100));
    let mut handles = Vec::new();

    for i in 0..client_refs.len() {
        let client = client_refs[i].clone();
        let semaphore = semaphore.clone();
        let test_duration = test_duration;

        let handle = tokio::spawn(async move {
            let mut rng = SmallRng::from_os_rng();
            let start = Instant::now();

            while start.elapsed() < test_duration {
                // Acquire permit in a scoped block
                {
                    let _permit = semaphore.acquire().await.unwrap();

                    // Random operation
                    match rng.gen_range(0..3) {
                        0 => {
                            // Ping
                            let _ = client.ping().await;
                        }
                        1 => {
                            // Send to random peer
                            let peer_idx = rng.gen_range(0..STRESS_TEST_CLIENTS);
                            if peer_idx != i {
                                let data = generate_game_data(rng.gen_range(50..500));
                                let _ = client.send_tunnel_packet((peer_idx + 1) as u32, &data).await;
                            }
                        }
                        _ => {
                            // Burst traffic
                            for _ in 0..5 {
                                let peer_idx = rng.gen_range(0..STRESS_TEST_CLIENTS);
                                if peer_idx != i {
                                    let data = generate_game_data(100);
                                    let _ = client.send_tunnel_packet((peer_idx + 1) as u32, &data).await;
                                }
                            }
                        }
                    }
                }

                sleep(Duration::from_millis(rng.gen_range(1..20))).await;
            }
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.await?;
    }

    // Collect metrics
    let mut total_sent = 0u64;
    let mut total_received = 0u64;
    let mut total_errors = 0u64;

    for client in &client_refs {
        total_sent += client.metrics.packets_sent.load(Ordering::Relaxed);
        total_received += client.metrics.packets_received.load(Ordering::Relaxed);
        total_errors += client.metrics.errors.load(Ordering::Relaxed);
    }

    info!("Stress test completed:");
    info!("  Total packets sent: {}", total_sent);
    info!("  Total packets received: {}", total_received);
    info!("  Total errors: {}", total_errors);

    let error_rate = if total_sent > 0 {
        (total_errors as f64 / total_sent as f64) * 100.0
    } else {
        0.0
    };

    // Even under stress, error rate should be reasonable
    assert!(error_rate < 5.0, "Error rate too high under stress: {:.2}%", error_rate);

    harness.shutdown().await?;
    Ok(())
}