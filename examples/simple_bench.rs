//! Simple benchmark for CnCNet server
//!
//! Tests basic operations without criterion for minimal dependencies

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

fn main() {
    println!("CnCNet Server Performance Benchmark");
    println!("===================================\n");

    bench_ip_hash();
    bench_packet_parsing();
    bench_address_obfuscation();
}

/// Benchmark IP hashing function
fn bench_ip_hash() {
    const ITERATIONS: u32 = 10_000_000;

    let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

    let start = Instant::now();
    let mut sum = 0i32;

    for _ in 0..ITERATIONS {
        // Simulate the IP hash function
        let hash = match ip {
            IpAddr::V4(v4) => {
                let octets = v4.octets();
                i32::from_ne_bytes(octets)
            }
            IpAddr::V6(v6) => {
                let segments = v6.segments();
                ((segments[0] as i32) << 16) | (segments[1] as i32)
            }
        };
        sum = sum.wrapping_add(hash);
    }

    let elapsed = start.elapsed();
    let ops_per_sec = ITERATIONS as f64 / elapsed.as_secs_f64();

    println!("IP Hash Performance:");
    println!("  Iterations: {}", ITERATIONS);
    println!("  Time: {:?}", elapsed);
    println!("  Ops/sec: {:.0}", ops_per_sec);
    println!("  ns/op: {:.2}", elapsed.as_nanos() as f64 / ITERATIONS as f64);
    println!("  Checksum: {}\n", sum);
}

/// Benchmark packet parsing
fn bench_packet_parsing() {
    const ITERATIONS: u32 = 10_000_000;

    let packet_v3 = vec![0x78, 0x56, 0x34, 0x12, 0x21, 0x43, 0x65, 0x87, 1, 2, 3, 4];
    let packet_v2 = vec![0x12, 0x34, 0x56, 0x78, 1, 2, 3, 4];

    // Benchmark V3 parsing
    let start = Instant::now();
    let mut sum = 0u64;

    for _ in 0..ITERATIONS {
        let sender_id = u32::from_le_bytes([packet_v3[0], packet_v3[1], packet_v3[2], packet_v3[3]]);
        let receiver_id = u32::from_le_bytes([packet_v3[4], packet_v3[5], packet_v3[6], packet_v3[7]]);
        sum = sum.wrapping_add(sender_id as u64).wrapping_add(receiver_id as u64);
    }

    let elapsed_v3 = start.elapsed();

    // Benchmark V2 parsing
    let start = Instant::now();
    let mut sum2 = 0i32;

    for _ in 0..ITERATIONS {
        let sender_id = i16::from_be_bytes([packet_v2[0], packet_v2[1]]);
        let receiver_id = i16::from_be_bytes([packet_v2[2], packet_v2[3]]);
        sum2 = sum2.wrapping_add(sender_id as i32).wrapping_add(receiver_id as i32);
    }

    let elapsed_v2 = start.elapsed();

    println!("Packet Parsing Performance:");
    println!("  V3 Parse:");
    println!("    Time: {:?}", elapsed_v3);
    println!("    Ops/sec: {:.0}", ITERATIONS as f64 / elapsed_v3.as_secs_f64());
    println!("    ns/op: {:.2}", elapsed_v3.as_nanos() as f64 / ITERATIONS as f64);
    println!("  V2 Parse:");
    println!("    Time: {:?}", elapsed_v2);
    println!("    Ops/sec: {:.0}", ITERATIONS as f64 / elapsed_v2.as_secs_f64());
    println!("    ns/op: {:.2}", elapsed_v2.as_nanos() as f64 / ITERATIONS as f64);
    println!("  Checksums: {} {}\n", sum, sum2);
}

/// Benchmark address obfuscation
fn bench_address_obfuscation() {
    const ITERATIONS: u32 = 10_000_000;

    let mut buffer = [192u8, 168, 1, 1, 0x1F, 0x90];

    let start = Instant::now();

    for _ in 0..ITERATIONS {
        // Obfuscate
        for i in 0..6 {
            buffer[i] ^= 0x20;
        }
        // Deobfuscate (to reset)
        for i in 0..6 {
            buffer[i] ^= 0x20;
        }
    }

    let elapsed = start.elapsed();

    println!("Address Obfuscation Performance:");
    println!("  Iterations: {} (obfuscate + deobfuscate)", ITERATIONS);
    println!("  Time: {:?}", elapsed);
    println!("  Ops/sec: {:.0}", ITERATIONS as f64 / elapsed.as_secs_f64());
    println!("  ns/op: {:.2}", elapsed.as_nanos() as f64 / ITERATIONS as f64);
    println!("  Final buffer: {:?}", buffer);
}