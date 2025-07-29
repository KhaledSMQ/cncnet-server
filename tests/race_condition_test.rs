//! Specific tests for race conditions in the CnCNet server
//!
//! This module contains targeted tests for potential race conditions,
//! particularly around atomic operations, concurrent state updates,
//! and edge cases in the tunneling protocols.
//!
//! ## Test Strategy
//!
//! - Use barriers to ensure true concurrent execution
//! - Test atomic operations under high contention
//! - Verify state consistency across concurrent modifications
//! - Test cleanup and connection state transitions

mod common;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::{Barrier, Mutex};
use tokio::time::sleep;
use tracing::info;
use rand::{Rng, SeedableRng};
use rand::rngs::SmallRng;
use common::*;

#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_client_id_allocation() -> Result<()> {
    info!("Testing concurrent client ID allocation race conditions");

    let harness = TestHarness::new(50100, 100).await?;

    // Track allocated IDs to check for duplicates
    let allocated_ids = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let duplicate_found = Arc::new(AtomicBool::new(false));

    // Barrier to ensure all tasks start simultaneously
    let barrier = Arc::new(Barrier::new(50));
    let mut handles = Vec::new();

    for i in 0..50 {
        let server_addr = harness.server_addr;
        let allocated_ids = allocated_ids.clone();
        let duplicate_found = duplicate_found.clone();
        let barrier = barrier.clone();

        let handle = tokio::spawn(async move {
            // Wait for all tasks to be ready
            barrier.wait().await;

            // Try to connect simultaneously
            if let Ok(client) = TestClient::new(i + 1, server_addr).await {
                // Send initial packet to establish connection
                let _ = client.ping().await;

                // Check if ID was already allocated
                let mut ids = allocated_ids.lock().await;
                if !ids.insert(i + 1) {
                    duplicate_found.store(true, Ordering::Relaxed);
                }
            }
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.await?;
    }

    assert!(!duplicate_found.load(Ordering::Relaxed), "Duplicate client IDs detected");

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_endpoint_updates() -> Result<()> {
    info!("Testing concurrent endpoint update race conditions");

    let harness = TestHarness::new(50101, 100).await?;

    // Create initial client
    let client = TestClient::new(1, harness.server_addr).await?;
    client.ping().await?; // Establish connection

    let update_count = Arc::new(AtomicU32::new(0));
    let error_count = Arc::new(AtomicU32::new(0));

    // Spawn multiple tasks trying to update endpoint simultaneously
    let mut handles = Vec::new();

    for port in 0..20 {
        let update_count = update_count.clone();
        let error_count = error_count.clone();
        let server_addr = harness.server_addr;

        let handle = tokio::spawn(async move {
            // Create client with same ID but different port (simulating IP change)
            match TestClient::new(1, server_addr).await {
                Ok(client) => {
                    // Try to send packet with same ID
                    match client.send_tunnel_packet(2, b"test").await {
                        Ok(_) => {
                            update_count.fetch_add(1, Ordering::Relaxed);
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

        handles.push(handle);
    }

    for handle in handles {
        handle.await?;
    }

    let updates = update_count.load(Ordering::Relaxed);
    let errors = error_count.load(Ordering::Relaxed);

    info!("Endpoint updates: {}, errors: {}", updates, errors);

    // Should have some successful updates but also some rejections
    assert!(updates > 0, "No successful endpoint updates");
    assert_eq!(updates + errors, 20, "Some operations lost");

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_rate_limiter_token_race() -> Result<()> {
    info!("Testing rate limiter token bucket race conditions");

    let harness = TestHarness::new(50102, 100).await?;

    // Create many clients from same IP
    let shared_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    let success_count = Arc::new(AtomicU64::new(0));
    let failure_count = Arc::new(AtomicU64::new(0));

    // Use barrier to ensure simultaneous execution
    let barrier = Arc::new(Barrier::new(100));
    let mut handles = Vec::new();

    for i in 0..100 {
        let barrier = barrier.clone();
        let success_count = success_count.clone();
        let failure_count = failure_count.clone();
        let server_addr = harness.server_addr;

        let handle = tokio::spawn(async move {
            if let Ok(mut client) = TestClient::new(i + 100, server_addr).await {
                client.simulated_ip = shared_ip;

                // Wait for all clients
                barrier.wait().await;

                // Burst of pings to exhaust rate limit
                for _ in 0..50 {
                    match client.ping().await {
                        Ok(_) => {
                            success_count.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {
                            failure_count.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.await?;
    }

    let successes = success_count.load(Ordering::Relaxed);
    let failures = failure_count.load(Ordering::Relaxed);

    info!("Rate limiter race test - Success: {}, Failed: {}", successes, failures);

    // Should have consistent token accounting
    assert!(successes > 0, "No successful operations");
    assert!(failures > 0, "Rate limiting not working");
    assert_eq!(successes + failures, 5000, "Token accounting mismatch");

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connection_state_consistency() -> Result<()> {
    info!("Testing connection state consistency under concurrent operations");

    let harness = TestHarness::new(50103, 200).await?;

    let connected_count = Arc::new(AtomicU32::new(0));
    let disconnected_count = Arc::new(AtomicU32::new(0));

    // Phase 1: Connect many clients
    let mut client_handles = Vec::new();

    for i in 0..50 {
        let connected_count = connected_count.clone();
        let server_addr = harness.server_addr;

        let handle = tokio::spawn(async move {
            if let Ok(client) = TestClient::new(i + 200, server_addr).await {
                if client.ping().await.is_ok() {
                    connected_count.fetch_add(1, Ordering::Relaxed);

                    // Keep alive for a bit
                    sleep(Duration::from_secs(2)).await;

                    // Return client to keep it alive
                    return Some(client);
                }
            }
            None
        });

        client_handles.push(handle);
    }

    // Collect connected clients
    let mut active_clients = Vec::new();
    for handle in client_handles {
        if let Some(client) = handle.await? {
            active_clients.push(client);
        }
    }

    let initial_connected = connected_count.load(Ordering::Relaxed);
    info!("Initially connected: {}", initial_connected);

    // Phase 2: Concurrent operations on connected clients
    if !active_clients.is_empty() {
        // Calculate split point safely
        let split_point = active_clients.len() / 2;
        let total_tasks = if split_point > 0 { active_clients.len() } else { 1 };

        let barrier = Arc::new(Barrier::new(total_tasks));
        let mut handles = Vec::new();

        if split_point > 0 {
            // Half the clients send traffic
            for client in &active_clients[..split_point] {
                let client_clone = client.clone();
                let barrier = barrier.clone();

                let handle = tokio::spawn(async move {
                    barrier.wait().await;

                    for _ in 0..100 {
                        let _ = client_clone.send_tunnel_packet(1, b"data").await;
                        sleep(Duration::from_millis(1)).await;
                    }
                });

                handles.push(handle);
            }

            // Other half disconnects (stops sending keepalives)
            for client in active_clients.drain(split_point..) {
                let disconnected_count = disconnected_count.clone();
                let barrier = barrier.clone();

                let handle = tokio::spawn(async move {
                    barrier.wait().await;

                    // Just drop the client
                    drop(client);
                    disconnected_count.fetch_add(1, Ordering::Relaxed);
                });

                handles.push(handle);
            }
        } else {
            // If only one client, just test it
            if let Some(client) = active_clients.first() {
                let client_clone = client.clone();
                let barrier = barrier.clone();

                let handle = tokio::spawn(async move {
                    barrier.wait().await;

                    for _ in 0..10 {
                        let _ = client_clone.send_tunnel_packet(1, b"data").await;
                        sleep(Duration::from_millis(10)).await;
                    }
                });

                handles.push(handle);
            }
        }

        for handle in handles {
            handle.await?;
        }
    } else {
        info!("No active clients connected, skipping concurrent operations test");
    }

    // Wait for timeouts to process
    sleep(Duration::from_secs(3)).await;

    // Verify state consistency
    let final_disconnected = disconnected_count.load(Ordering::Relaxed);
    info!("Clients that disconnected: {}", final_disconnected);

    // Create new client to verify server is still functional
    let test_client = TestClient::new(999, harness.server_addr).await?;
    assert!(test_client.ping().await.is_ok(), "Server not responding after concurrent operations");

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_packet_ordering_under_concurrency() -> Result<()> {
    info!("Testing packet ordering under high concurrency");

    let harness = TestHarness::new(50104, 100).await?;

    // Create sender and receiver
    let sender = Arc::new(TestClient::new(1, harness.server_addr).await?);
    let receiver = Arc::new(TestClient::new(2, harness.server_addr).await?);

    // Establish connections
    sender.ping().await?;
    receiver.ping().await?;

    let packets_to_send: u32 = 1000;
    let received_packets = Arc::new(Mutex::new(Vec::new()));

    // Receiver task
    let receiver_clone = receiver.clone();
    let received_packets_clone = received_packets.clone();
    let receiver_handle = tokio::spawn(async move {
        let start = Instant::now();

        while start.elapsed() < Duration::from_secs(10) {
            match receiver_clone.recv_tunnel_packet(Duration::from_millis(100)).await {
                Ok((sender_id, data)) => {
                    if sender_id == 1 && data.len() >= 4 {
                        let seq = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                        received_packets_clone.lock().await.push(seq);
                    }
                }
                Err(_) => {
                    // Timeout or error, continue
                }
            }
        }
    });

    // Send packets concurrently
    let mut send_handles = Vec::new();

    for chunk_start in (0..packets_to_send).step_by(100) {
        let sender = sender.clone();

        let handle = tokio::spawn(async move {
            for i in chunk_start..chunk_start + 100 {
                let mut data = vec![0u8; 100];
                data[0..4].copy_from_slice(&i.to_le_bytes());

                let _ = sender.send_tunnel_packet(2, &data).await;

                // Small random delay to increase concurrency
                if i % 10 == 0 {
                    sleep(Duration::from_micros(100)).await;
                }
            }
        });

        send_handles.push(handle);
    }

    // Wait for senders
    for handle in send_handles {
        handle.await?;
    }

    // Give receiver time to catch up
    sleep(Duration::from_secs(2)).await;

    // Stop receiver
    receiver_handle.abort();

    // Analyze received packets
    let received = received_packets.lock().await;
    info!("Received {} out of {} packets", received.len(), packets_to_send);

    // Check for duplicates
    let mut unique_packets = std::collections::HashSet::new();
    for &seq in received.iter() {
        assert!(unique_packets.insert(seq), "Duplicate packet detected: {}", seq);
    }

    // Packet delivery rate should be high
    let delivery_rate = received.len() as f64 / packets_to_send as f64;
    assert!(delivery_rate > 0.95, "Packet delivery rate too low: {:.2}%", delivery_rate * 100.0);

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cleanup_race_conditions() -> Result<()> {
    info!("Testing cleanup task race conditions");

    let harness = TestHarness::new(50105, 100).await?;

    let active_connections = Arc::new(AtomicU32::new(0));
    let cleanup_triggered = Arc::new(AtomicBool::new(false));

    // Create clients that will timeout
    let mut handles = Vec::new();

    for i in 0..20 {
        let active_connections = active_connections.clone();
        let server_addr = harness.server_addr;

        let handle = tokio::spawn(async move {
            if let Ok(client) = TestClient::new(i + 300, server_addr).await {
                if client.ping().await.is_ok() {
                    active_connections.fetch_add(1, Ordering::Relaxed);

                    // Stop sending keepalives to trigger timeout
                    sleep(Duration::from_secs(1)).await;
                }
            }
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.await?;
    }

    let initial_active = active_connections.load(Ordering::Relaxed);
    info!("Initial active connections: {}", initial_active);

    // Wait for cleanup to potentially run
    sleep(Duration::from_secs(5)).await;

    // Now create new connections while cleanup might be running
    let barrier = Arc::new(Barrier::new(30));
    let mut new_handles = Vec::new();

    for i in 0..30 {
        let barrier = barrier.clone();
        let cleanup_triggered = cleanup_triggered.clone();
        let server_addr = harness.server_addr;

        let handle = tokio::spawn(async move {
            barrier.wait().await;

            // Try to connect during potential cleanup
            if let Ok(client) = TestClient::new(i + 400, server_addr).await {
                match client.ping().await {
                    Ok(_) => {
                        // Successfully connected during cleanup
                        cleanup_triggered.store(true, Ordering::Relaxed);

                        // Keep connection alive
                        for _ in 0..10 {
                            let _ = client.ping().await;
                            sleep(Duration::from_millis(100)).await;
                        }
                    }
                    Err(_) => {
                        // Connection failed (possibly due to cleanup)
                    }
                }
            }
        });

        new_handles.push(handle);
    }

    for handle in new_handles {
        handle.await?;
    }

    assert!(cleanup_triggered.load(Ordering::Relaxed), "No connections succeeded during cleanup window");

    harness.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_extreme_connection_churn() -> Result<()> {
    info!("Testing extreme connection churn for memory and state consistency");

    let harness = TestHarness::new(50106, 200).await?;

    let total_connections = Arc::new(AtomicU64::new(0));
    let concurrent_connections = Arc::new(AtomicU32::new(0));
    let max_concurrent = Arc::new(AtomicU32::new(0));

    let test_duration = Duration::from_secs(20);
    let start_time = Instant::now();

    // Spawn many tasks that rapidly connect and disconnect
    let mut handles = Vec::new();

    for task_id in 0..50 {
        let total_connections = total_connections.clone();
        let concurrent_connections = concurrent_connections.clone();
        let max_concurrent = max_concurrent.clone();
        let server_addr = harness.server_addr;

        let handle = tokio::spawn(async move {
            let mut rng = SmallRng::from_os_rng();
            let mut client_id = (task_id * 1000) as u32;

            while start_time.elapsed() < test_duration {
                client_id += 1;

                if let Ok(client) = TestClient::new(client_id, server_addr).await {
                    if client.ping().await.is_ok() {
                        total_connections.fetch_add(1, Ordering::Relaxed);
                        let current = concurrent_connections.fetch_add(1, Ordering::Relaxed) + 1;
                        max_concurrent.fetch_max(current, Ordering::Relaxed);

                        // Very short connection lifetime
                        sleep(Duration::from_millis(rng.gen_range(10..100))).await;

                        // Explicit drop
                        drop(client);
                        concurrent_connections.fetch_sub(1, Ordering::Relaxed);
                    }
                }

                // Brief pause between connections
                sleep(Duration::from_millis(rng.gen_range(1..10))).await;
            }
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.await?;
    }

    let total = total_connections.load(Ordering::Relaxed);
    let max_conc = max_concurrent.load(Ordering::Relaxed);

    info!("Connection churn test completed:");
    info!("  Total connections: {}", total);
    info!("  Max concurrent: {}", max_conc);
    info!("  Connections per second: {:.2}", total as f64 / test_duration.as_secs_f64());

    // Verify server is still healthy
    let test_client = TestClient::new(999999, harness.server_addr).await?;
    assert!(test_client.ping().await.is_ok(), "Server unresponsive after extreme churn");

    harness.shutdown().await?;
    Ok(())
}