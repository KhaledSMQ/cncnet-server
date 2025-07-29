//! Example client for testing CnCNet server
//!
//! This client tests all server endpoints and protocols to ensure
//! compatibility with the C# server implementation.
//!
//! Run with: cargo run --example test_client

use std::net::UdpSocket;
use std::time::Duration;

/// Entry point for the test client
///
/// Tests all server protocols in sequence:
/// - V3 tunnel ping
/// - V2 tunnel ping
/// - P2P STUN
/// - V2 HTTP status
fn main() {
    println!("CnCNet Test Client");
    println!("==================");

    // Test V3 tunnel ping
    println!("\nTesting Tunnel V3 ping...");
    match test_v3_ping() {
        Ok(()) => println!("V3 ping successful!"),
        Err(e) => println!("V3 ping failed: {}", e),
    }

    // Test V2 tunnel ping
    println!("\nTesting Tunnel V2 ping...");
    match test_v2_ping() {
        Ok(()) => println!("V2 ping successful!"),
        Err(e) => println!("V2 ping failed: {}", e),
    }

    // Test P2P STUN
    println!("\nTesting P2P STUN...");
    match test_p2p_stun() {
        Ok(()) => println!("P2P STUN successful!"),
        Err(e) => println!("P2P STUN failed: {}", e),
    }

    // Test V2 HTTP status
    println!("\nTesting V2 HTTP status...");
    match test_v2_http_status() {
        Ok(()) => println!("V2 HTTP status successful!"),
        Err(e) => println!("V2 HTTP status failed: {}", e),
    }
}

/// Tests V3 tunnel ping functionality
///
/// Protocol:
/// - Send 50-byte packet with sender_id=0, receiver_id=0
/// - Expect 12-byte response
fn test_v3_ping() -> Result<(), Box<dyn std::error::Error>> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;

    // Get local IP by connecting to an external address
    let temp_socket = UdpSocket::bind("0.0.0.0:0")?;
    temp_socket.connect("8.8.8.8:53")?;
    let local_ip = temp_socket.local_addr()?.ip();
    let server_addr = format!("{}:50001", local_ip);

    // Create ping packet - must be exactly 50 bytes
    let ping = vec![0u8; 50];

    // Send ping to V3 port using local IP
    socket.send_to(&ping, server_addr)?;

    // Receive response
    let mut buf = vec![0u8; 64];
    let (size, from) = socket.recv_from(&mut buf)?;

    println!("  Received {} bytes from {}", size, from);

    // Verify response format
    if size != 12 {
        return Err(format!("Expected 12 bytes, got {}", size).into());
    }

    Ok(())
}

/// Tests V2 tunnel ping functionality
///
/// Protocol:
/// - Send 50-byte packet with sender_id=0, receiver_id=0
/// - Expect 12-byte response
fn test_v2_ping() -> Result<(), Box<dyn std::error::Error>> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;

    // Get local IP by connecting to an external address
    let temp_socket = UdpSocket::bind("0.0.0.0:0")?;
    temp_socket.connect("8.8.8.8:53")?;
    let local_ip = temp_socket.local_addr()?.ip();
    let server_addr = format!("{}:50000", local_ip);

    // Create ping packet - must be exactly 50 bytes
    let ping = vec![0u8; 50];

    // Send ping to V2 port using local IP
    socket.send_to(&ping, server_addr)?;

    // Receive response
    let mut buf = vec![0u8; 64];
    let (size, from) = socket.recv_from(&mut buf)?;

    println!("  Received {} bytes from {}", size, from);

    // Verify response format
    if size != 12 {
        return Err(format!("Expected 12 bytes, got {}", size).into());
    }

    Ok(())
}

/// Tests P2P STUN functionality
///
/// Protocol:
/// - Send 48-byte request with STUN ID (26262)
/// - Expect 40-byte response with obfuscated address
fn test_p2p_stun() -> Result<(), Box<dyn std::error::Error>> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;

    // Get local IP by connecting to an external address
    let temp_socket = UdpSocket::bind("0.0.0.0:0")?;
    temp_socket.connect("8.8.8.8:53")?;
    let local_ip = temp_socket.local_addr()?.ip();
    let server_addr = format!("{}:8054", local_ip);

    // Create STUN request - must be exactly 48 bytes
    let mut stun_request = vec![0u8; 48];

    // Set STUN ID (26262 = 0x6696) in big-endian format
    stun_request[0] = 0x66; // High byte
    stun_request[1] = 0x96; // Low byte

    // Send to P2P port using local IP
    socket.send_to(&stun_request, server_addr)?;

    // Receive response
    let mut buf = vec![0u8; 64];
    let (size, from) = socket.recv_from(&mut buf)?;

    println!("  Received {} bytes from {}", size, from);

    // Verify response size
    if size != 40 {
        return Err(format!("Expected 40 bytes, got {}", size).into());
    }

    // Decode obfuscated address (XOR with 0x20)
    let ip = format!("{}.{}.{}.{}",
                     buf[0] ^ 0x20,
                     buf[1] ^ 0x20,
                     buf[2] ^ 0x20,
                     buf[3] ^ 0x20
    );
    let port = u16::from_be_bytes([buf[4] ^ 0x20, buf[5] ^ 0x20]);

    println!("  External address: {}:{}", ip, port);

    Ok(())
}

/// Tests V2 HTTP status endpoint
///
/// Protocol:
/// - GET /status
/// - Expect status text with slot information
fn test_v2_http_status() -> Result<(), Box<dyn std::error::Error>> {
    // Use curl for simplicity (could use reqwest in a real client)
    let output = std::process::Command::new("curl")
        .arg("-s")
        .arg("-f") // Fail on HTTP errors
        .arg("http://127.0.0.1:50000/status")
        .output()?;

    if !output.status.success() {
        return Err("HTTP request failed".into());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    println!("  Status: {}", body.trim());

    // Verify response format
    if !body.contains("slots free") || !body.contains("slots in use") {
        return Err("Invalid status response format".into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stun_id_encoding() {
        // Verify STUN ID encoding
        const STUN_ID: i16 = 26262;
        let bytes = STUN_ID.to_be_bytes();
        assert_eq!(bytes[0], 0x66);
        assert_eq!(bytes[1], 0x96);
    }

    #[test]
    fn test_address_obfuscation() {
        // Test XOR obfuscation
        let ip_bytes = [127, 0, 0, 1];
        let obfuscated: Vec<u8> = ip_bytes.iter().map(|&b| b ^ 0x20).collect();
        let deobfuscated: Vec<u8> = obfuscated.iter().map(|&b| b ^ 0x20).collect();

        assert_eq!(ip_bytes.to_vec(), deobfuscated);
    }
}