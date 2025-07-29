//! Performance benchmarks for CnCNet server components
//!
//! This benchmark suite measures the performance characteristics of critical
//! server components to ensure they meet the requirements for low memory usage,
//! high throughput, and minimal latency.
//!
//! ## Benchmark Categories
//!
//! 1. **Rate Limiter Performance**: Lock-free operations, concurrent access
//! 2. **Packet Processing**: UDP packet handling throughput
//! 3. **Connection Management**: Client lookup and state management
//! 4. **Memory Usage**: Allocation patterns and memory footprint
//! 5. **Concurrent Load**: Performance under high concurrency
//!
//! ## Running Benchmarks
//!
//! ```bash
//! cargo bench
//! cargo bench -- --save-baseline baseline
//! cargo bench -- --baseline baseline
//! ```

use criterion::{  criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;

use cncnet_server::net::{
    peer_to_peer::{PeerToPeerService, PeerToPeerConfig},
    rate_limiter::{IpRateLimiterManager, RateLimiter, RateLimiterConfig, MemoryOrdering},
    tunnel_v2::{TunnelV2, TunnelV2Config},
    tunnel_v3::{TunnelV3, TunnelV3Config},
};

/// Benchmarks rate limiter performance under various scenarios
fn bench_rate_limiter(c: &mut Criterion) {
    let mut group = c.benchmark_group("rate_limiter");

    // Single-threaded token acquisition
    group.bench_function("single_thread_acquire", |b| {
        let limiter = RateLimiter::new(1000, 100);
        b.iter(|| {
            std::hint::black_box(limiter.try_acquire())
        });
    });

    // Multi-threaded token acquisition
    group.bench_function("multi_thread_acquire", |b| {
        let limiter = Arc::new(RateLimiter::new(10000, 1000));
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();

            // Spawn multiple threads
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let limiter = limiter.clone();
                    let iters_per_thread = iters / 4;
                    std::thread::spawn(move || {
                        for _ in 0..iters_per_thread {
                            std::hint::black_box(limiter.try_acquire());
                        }
                    })
                })
                .collect();

            // Wait for all threads
            for handle in handles {
                handle.join().unwrap();
            }

            start.elapsed()
        });
    });

    // Batch token acquisition
    group.bench_function("batch_acquire", |b| {
        let limiter = RateLimiter::new(10000, 1000);
        b.iter(|| {
            std::hint::black_box(limiter.try_acquire_n(10))
        });
    });

    // Token refill performance
    group.bench_function("token_refill", |b| {
        let config = RateLimiterConfig {
            max_tokens: 100,
            refill_rate: 50,
            refill_interval_ms: 1, // Very fast refill for benchmark
            ordering: MemoryOrdering::Relaxed,
        };
        let limiter = RateLimiter::with_config(config);

        // Consume all tokens
        for _ in 0..100 {
            limiter.try_acquire();
        }

        b.iter(|| {
            // This will trigger refill check
            std::hint::black_box(limiter.available_tokens())
        });
    });

    // Different memory ordering impact
    for ordering in [MemoryOrdering::Relaxed, MemoryOrdering::AcquireRelease, MemoryOrdering::Sequential] {
        let name = format!("ordering_{:?}", ordering);
        group.bench_function(&name, |b| {
            let config = RateLimiterConfig {
                max_tokens: 1000,
                refill_rate: 100,
                refill_interval_ms: 1000,
                ordering,
            };
            let limiter = RateLimiter::with_config(config);
            b.iter(|| {
                std::hint::black_box(limiter.try_acquire())
            });
        });
    }

    group.finish();
}

/// Benchmarks IP-based rate limiter manager
fn bench_ip_rate_limiter(c: &mut Criterion) {
    let mut group = c.benchmark_group("ip_rate_limiter");

    // IP lookup and rate limiting
    group.bench_function("ip_lookup_and_acquire", |b| {
        let config = RateLimiterConfig::default();
        let manager = IpRateLimiterManager::new(config);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        b.iter(|| {
            std::hint::black_box(manager.try_acquire(ip))
        });
    });

    // Multiple IPs
    let ip_counts = [10, 100, 1000];
    for count in ip_counts {
        group.throughput(Throughput::Elements(count));
        group.bench_with_input(
            BenchmarkId::new("multiple_ips", count),
            &count,
            |b, &count| {
                let config = RateLimiterConfig::default();
                let manager = IpRateLimiterManager::new(config);
                let ips: Vec<IpAddr> = (0..count)
                    .map(|i| IpAddr::V4(Ipv4Addr::new(192, 168, (i / 256) as u8, (i % 256) as u8)))
                    .collect();

                b.iter(|| {
                    for &ip in &ips {
                        std::hint::black_box(manager.try_acquire(ip));
                    }
                });
            },
        );
    }

    // Cleanup performance
    group.bench_function("cleanup_inactive", |b| {
        let config = RateLimiterConfig::default();
        let manager = IpRateLimiterManager::new(config);

        // Pre-populate with IPs
        for i in 0..100 {
            let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, i));
            manager.try_acquire(ip);
        }

        b.iter(|| {
            manager.cleanup();
        });
    });

    group.finish();
}

/// Benchmarks P2P service packet processing
fn bench_p2p_service(c: &mut Criterion) {
    let mut group = c.benchmark_group("p2p_service");
    let rt = Runtime::new().unwrap();

    // Valid source address checking
    group.bench_function("valid_source_check", |b| {
        let service = PeerToPeerService::new(8054);
        let addr: SocketAddr = "192.168.1.1:1234".parse().unwrap();

        b.iter(|| {
             std::hint::black_box(service.is_valid_source_address(&addr))
        });
    });

    // Rate limit checking
    group.bench_function("rate_limit_check", |b| {
        let config = PeerToPeerConfig {
            port: 8054,
            max_requests_per_ip: 100,
            max_requests_global: 1000,
            reset_interval_ms: 60000,
            max_concurrent_processing: 1000,
        };
        let service = PeerToPeerService::with_config(config);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        b.iter(|| {
            std::hint::black_box(service.check_rate_limits(ip))
        });
    });

    // // Buffer pool performance
    // group.bench_function("buffer_pool_get", |b| {
    //     let service = PeerToPeerService::new(8054);
    // 
    //     b.iter(|| {
    //         std::hint::black_box(service.buffer_pool.get_buffer())
    //     });
    // });

    group.finish();
}

/// Benchmarks Tunnel V2 operations
fn bench_tunnel_v2(c: &mut Criterion) {
    let mut group = c.benchmark_group("tunnel_v2");

    // Packet validation
    group.bench_function("packet_validation", |b| {
        let config = TunnelV2Config::default();
        let service = TunnelV2::with_config(config);
        let addr: SocketAddr = "192.168.1.1:1234".parse().unwrap();

        b.iter(|| {
            std::hint::black_box(service.is_valid_packet(1, 2, &addr))
        });
    });

    // Ping rate limiting
    group.bench_function("ping_rate_limit", |b| {
        let config = TunnelV2Config {
            max_pings_per_ip: 100,
            max_pings_global: 1000,
            ..Default::default()
        };
        let service = TunnelV2::with_config(config);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        b.iter(|| {
            std::hint::black_box(service.ping_limit_reached(ip))
        });
    });

    // Client ID allocation
    group.bench_function("allocate_client_ids", |b| {
        use dashmap::DashMap;
        use cncnet_server::net::tunnel_client::TunnelClient;

        b.iter_batched(
            || DashMap::new(),
            |mappings| {
                std::hint::black_box(TunnelV2::allocate_client_ids(&mappings, 4, 200))
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}
//
// /// Benchmarks Tunnel V3 operations
// fn bench_tunnel_v3(c: &mut Criterion) {
//     let mut group = c.benchmark_group("tunnel_v3");
//
//     // Packet validation
//     group.bench_function("packet_validation", |b| {
//         let config = TunnelV3Config::default();
//         let service = TunnelV3::with_config(config);
//         let addr: SocketAddr = "192.168.1.1:1234".parse().unwrap();
//
//         b.iter(|| {
//             std::hint::black_box(service.is_valid_packet(1, 2, &addr))
//         });
//     });
//
//     // Connection state operations
//     group.bench_function("connection_add", |b| {
//         use cncnet_server::net::tunnel_v3::ConnectionState;
//
//         let state = ConnectionState::new();
//         let mut client_id = 0u32;
//         let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
//
//         b.iter(|| {
//             client_id = client_id.wrapping_add(1);
//             std::hint::black_box(state.try_add_connection(client_id, ip, 8))
//         });
//     });
//
//     group.bench_function("connection_update", |b| {
//         use cncnet_server::net::tunnel_v3::ConnectionState;
//
//         let state = ConnectionState::new();
//         let client_id = 12345u32;
//         let ip1 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
//         let ip2 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));
//
//         // Pre-add connection
//         state.try_add_connection(client_id, ip1, 8);
//
//         b.iter(|| {
//             std::hint::black_box(state.update_connection(client_id, ip2, 8));
//             std::hint::black_box(state.update_connection(client_id, ip1, 8));
//         });
//     });
//
//     group.finish();
// }

/// Benchmarks memory allocation patterns
fn bench_memory_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_patterns");

    // SmallVec vs Vec for packet data
    group.bench_function("smallvec_64_bytes", |b| {
        use smallvec::SmallVec;
        let data = vec![0u8; 64];

        b.iter(|| {
            let sv: SmallVec<[u8; 64]> = data.iter().copied().collect();
            std::hint::black_box(sv)
        });
    });

    group.bench_function("vec_64_bytes", |b| {
        let data = vec![0u8; 64];

        b.iter(|| {
            let v: Vec<u8> = data.iter().copied().collect();
            std::hint::black_box(v)
        });
    });

    // DashMap vs standard HashMap operations
    group.bench_function("dashmap_insert_lookup", |b| {
        use dashmap::DashMap;
        let map = DashMap::new();
        let mut key = 0u32;

        b.iter(|| {
            key = key.wrapping_add(1);
            map.insert(key, key * 2);
            std::hint::black_box(map.get(&key));
        });
    });

    group.finish();
}

/// Benchmarks concurrent load scenarios
fn bench_concurrent_load(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_load");
    group.sample_size(30); // Reduce sample size for longer benchmarks
    group.measurement_time(Duration::from_secs(10));

    // Simulate concurrent client connections
    let client_counts = [100, 500, 1000];

    for count in client_counts {
        group.throughput(Throughput::Elements(count));
        group.bench_with_input(
            BenchmarkId::new("concurrent_clients", count),
            &count,
            |b, &count| {
                let rt = Runtime::new().unwrap();

                b.to_async(&rt).iter(|| async move {
                    use tokio::task::JoinSet;
                    let mut set = JoinSet::new();

                    // Simulate concurrent client operations
                    for i in 0..count {
                        set.spawn(async move {
                            // Simulate some work
                            tokio::time::sleep(Duration::from_micros(10)).await;
                            std::hint::black_box(i)
                        });
                    }

                    // Wait for all tasks
                    while let Some(_) = set.join_next().await {}
                });
            },
        );
    }

    group.finish();
}

/// Benchmarks packet forwarding performance
fn bench_packet_forwarding(c: &mut Criterion) {
    let mut group = c.benchmark_group("packet_forwarding");

    // Different packet sizes
    let packet_sizes = [64, 256, 512, 1024];

    for size in packet_sizes {
        group.throughput(Throughput::Bytes(size));
        group.bench_with_input(
            BenchmarkId::new("forward_packet", size),
            &size,
            |b, &size| {
                let rt = Runtime::new().unwrap();
                let packet = vec![0u8; size as usize];

                b.to_async(&rt).iter(|| async {
                    // Simulate packet processing without actual network I/O
                    let processed = packet.clone();
                    std::hint::black_box(processed)
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_rate_limiter,
    bench_ip_rate_limiter,
    bench_p2p_service,
    bench_tunnel_v2,
    // bench_tunnel_v3,
    bench_memory_patterns,
    bench_concurrent_load,
    bench_packet_forwarding
);

criterion_main!(benches);