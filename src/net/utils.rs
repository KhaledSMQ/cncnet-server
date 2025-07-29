//! Network utility functions for socket configuration and optimization
//!
//! This module provides common utilities for configuring sockets
//! for optimal performance and reliability.

use std::net::SocketAddr;
use tokio::net::UdpSocket;
use anyhow::Result;
use tracing::{info, warn};

/// Creates and configures a UDP socket for server use
///
/// Features:
/// - SO_REUSEADDR for quick restart
/// - Optimal buffer sizes for game traffic
/// - Non-blocking mode
///
/// # Arguments
///
/// * `addr` - Address to bind to
///
/// # Returns
///
/// Configured UDP socket ready for use
pub async fn create_optimized_socket(addr: SocketAddr) -> Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    // Create socket with socket2 for more control
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;

    // Enable SO_REUSEADDR for quick restart after crash
    socket.set_reuse_address(true)?;

    // Set socket buffer sizes for better performance
    // 256KB receive buffer for burst traffic
    match socket.set_recv_buffer_size(256 * 1024) {
        Ok(_) => info!("Set receive buffer to 256KB"),
        Err(e) => warn!("Could not set receive buffer size: {}", e),
    }

    // 128KB send buffer
    match socket.set_send_buffer_size(128 * 1024) {
        Ok(_) => info!("Set send buffer to 128KB"),
        Err(e) => warn!("Could not set send buffer size: {}", e),
    }

    // Bind the socket
    socket.bind(&addr.into())?;

    // Set non-blocking mode
    socket.set_nonblocking(true)?;

    // Convert to tokio UdpSocket
    let std_socket: std::net::UdpSocket = socket.into();
    let tokio_socket = UdpSocket::from_std(std_socket)?;

    info!("Socket bound to {} with optimized settings", addr);

    Ok(tokio_socket)
} 