//! Network utility functions for socket configuration and optimization

use std::net::SocketAddr;
use tokio::net::UdpSocket;
use anyhow::{Result, Context};
use tracing::{info, warn, error, debug};

/// Minimum acceptable socket buffer size (32KB)
const MIN_SOCKET_BUFFER_SIZE: usize = 32 * 1024;

/// Minimum recommended socket buffer size for good performance (64KB)
const RECOMMENDED_SOCKET_BUFFER_SIZE: usize = 64 * 1024;

/// Creates and configures a UDP socket for server use with fallback options
///
/// Features:
/// - SO_REUSEADDR for quick restart
/// - Optimal buffer sizes for game traffic (with fallbacks)
/// - Non-blocking mode
/// - Graceful degradation on permission errors
pub async fn create_optimized_socket(addr: SocketAddr) -> Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    // Create socket with socket2 for more control
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))
        .context("Failed to create UDP socket")?;

    // Enable SO_REUSEADDR for quick restart
    socket.set_reuse_address(true)
        .context("Failed to set SO_REUSEADDR")?;

    // On Unix systems, also try to set SO_REUSEPORT for better port reuse
    #[cfg(unix)]
    {
        if let Err(e) = socket.set_reuse_port(true) {
            warn!("Failed to set SO_REUSEPORT: {} (non-critical)", e);
        }
    }

    // Try to set socket buffer sizes with fallback logic
    set_socket_buffers(&socket)?;

    // Bind the socket
    socket.bind(&addr.into())
        .with_context(|| format!("Failed to bind socket to {}", addr))?;

    // Set non-blocking mode
    socket.set_nonblocking(true)
        .context("Failed to set non-blocking mode")?;

    // Convert to tokio UdpSocket
    let std_socket: std::net::UdpSocket = socket.into();
    let tokio_socket = UdpSocket::from_std(std_socket)
        .context("Failed to convert to Tokio socket")?;

    info!("Socket bound to {} with optimized settings", addr);

    Ok(tokio_socket)
}

/// Sets socket buffer sizes with fallback logic for different permission levels
fn set_socket_buffers(socket: &socket2::Socket) -> Result<()> {
    // Define buffer sizes to try (from optimal to minimum)
    const BUFFER_SIZES: &[(usize, &str)] = &[
        (256 * 1024, "256KB (optimal)"),
        (128 * 1024, "128KB (good)"),
        (64 * 1024, "64KB (acceptable)"),
        (32 * 1024, "32KB (minimum)"),
    ];

    // Try to set receive buffer
    let mut recv_set = false;
    let mut recv_size = 0;
    for &(size, desc) in BUFFER_SIZES {
        match socket.set_recv_buffer_size(size) {
            Ok(_) => {
                info!("Set receive buffer to {}", desc);
                recv_set = true;
                recv_size = size;
                break;
            }
            Err(e) => {
                if size == BUFFER_SIZES.last().unwrap().0 {
                    error!("Failed to set any receive buffer size: {}", e);
                } else {
                    warn!("Could not set receive buffer to {}: {}", desc, e);
                }
            }
        }
    }

    // Try to set send buffer (typically needs less than receive)
    const SEND_SIZES: &[(usize, &str)] = &[
        (128 * 1024, "128KB"),
        (64 * 1024, "64KB"),
        (32 * 1024, "32KB"),
    ];

    let mut send_set = false;
    let mut send_size = 0;
    for &(size, desc) in SEND_SIZES {
        match socket.set_send_buffer_size(size) {
            Ok(_) => {
                info!("Set send buffer to {}", desc);
                send_set = true;
                send_size = size;
                break;
            }
            Err(e) => {
                if size == SEND_SIZES.last().unwrap().0 {
                    error!("Failed to set any send buffer size: {}", e);
                } else {
                    warn!("Could not set send buffer to {}: {}", desc, e);
                }
            }
        }
    }

    // Check actual buffer sizes if possible
    let actual_recv = socket.recv_buffer_size().unwrap_or(0);
    let actual_send = socket.send_buffer_size().unwrap_or(0);

    if actual_recv > 0 {
        info!("Actual receive buffer size: {} bytes", actual_recv);
    }
    if actual_send > 0 {
        info!("Actual send buffer size: {} bytes", actual_send);
    }

    // Determine if we should fail or warn based on what we achieved
    if !recv_set && !send_set {
        // Complete failure - couldn't set any buffers
        error!("Failed to set any socket buffer sizes!");
        error!("Consider running with elevated privileges or adjusting system limits:");
        error!("  Linux: sysctl -w net.core.rmem_max=262144");
        error!("  Linux: sysctl -w net.core.wmem_max=262144");
        error!("  Windows: Run as Administrator");
        error!("  macOS: sudo sysctl -w kern.ipc.maxsockbuf=262144");
        anyhow::bail!("Socket buffer configuration failed completely");
    } else if actual_recv > 0 && actual_recv < MIN_SOCKET_BUFFER_SIZE {
        // Receive buffer is critically small
        error!(
            "Receive buffer size {} is below minimum required {} bytes",
            actual_recv, MIN_SOCKET_BUFFER_SIZE
        );
        error!("Server performance will be severely impacted!");
        error!("Please increase system socket buffer limits");
        // Don't fail, but warn severely
        warn!("Continuing with suboptimal buffer size - expect performance issues");
    } else if recv_size < RECOMMENDED_SOCKET_BUFFER_SIZE || send_size < RECOMMENDED_SOCKET_BUFFER_SIZE {
        // We got buffers set, but they're not optimal
        warn!(
            "Socket buffers are below recommended size ({}KB receive, {}KB send)",
            RECOMMENDED_SOCKET_BUFFER_SIZE / 1024,
            RECOMMENDED_SOCKET_BUFFER_SIZE / 1024
        );
        warn!("Performance may be impacted under heavy load");
        info!("To improve performance, increase system socket buffer limits");
    }

    Ok(())
}

/// Gets system socket buffer limits for diagnostics
#[cfg(target_os = "linux")]
pub fn get_system_socket_limits() -> Result<(usize, usize)> {
    use std::fs;

    let rmem_max = fs::read_to_string("/proc/sys/net/core/rmem_max")?
        .trim()
        .parse::<usize>()?;
    let wmem_max = fs::read_to_string("/proc/sys/net/core/wmem_max")?
        .trim()
        .parse::<usize>()?;

    Ok((rmem_max, wmem_max))
}

#[cfg(target_os = "macos")]
pub fn get_system_socket_limits() -> Result<(usize, usize)> {
    use std::process::Command;

    // On macOS, use sysctl to get the values
    let output = Command::new("sysctl")
        .arg("-n")
        .arg("kern.ipc.maxsockbuf")
        .output()?;

    let max_buf = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<usize>()
        .unwrap_or(0);

    // macOS uses the same limit for both send and receive
    Ok((max_buf, max_buf))
}

#[cfg(target_os = "windows")]
pub fn get_system_socket_limits() -> Result<(usize, usize)> {
    // Windows doesn't have easily accessible system-wide limits
    // Return a reasonable default that Windows typically supports
    Ok((65536, 65536)) // 64KB default on Windows
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub fn get_system_socket_limits() -> Result<(usize, usize)> {
    // Return dummy values for other systems
    Ok((0, 0))
}

/// Logs system socket limits for diagnostics
pub fn log_socket_limits() {
    match get_system_socket_limits() {
        Ok((rmem, wmem)) => {
            if rmem > 0 && wmem > 0 {
                info!("System socket buffer limits - receive: {} bytes, send: {} bytes", rmem, wmem);

                if rmem < 256 * 1024 || wmem < 128 * 1024 {
                    warn!("System socket buffer limits are low, consider increasing them:");

                    #[cfg(target_os = "linux")]
                    {
                        warn!("  sudo sysctl -w net.core.rmem_max=262144");
                        warn!("  sudo sysctl -w net.core.wmem_max=262144");
                        warn!("  To make permanent, add to /etc/sysctl.conf");
                    }

                    #[cfg(target_os = "macos")]
                    {
                        warn!("  sudo sysctl -w kern.ipc.maxsockbuf=262144");
                        warn!("  To make permanent, add to /etc/sysctl.conf");
                    }

                    #[cfg(target_os = "windows")]
                    {
                        warn!("  Windows: Socket buffer sizes are typically sufficient");
                        warn!("  If needed, run the server as Administrator");
                    }
                }
            }
        }
        Err(e) => {
            // Not critical if we can't read limits
            debug!("Could not read system socket limits: {}", e);
        }
    }
}

/// Helper function to format byte sizes in human-readable format
pub fn format_bytes(bytes: usize) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{} {}", bytes, UNITS[unit_index])
    } else {
        format!("{:.1} {}", size, UNITS[unit_index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[tokio::test]
    async fn test_create_socket() {
        // Use port 0 for automatic assignment
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let socket = create_optimized_socket(addr).await.unwrap();

        // Should be able to get local address
        let local_addr = socket.local_addr().unwrap();
        assert!(local_addr.port() > 0);
    }

    #[test]
    fn test_system_limits() {
        // Just test that the function doesn't panic
        let _ = get_system_socket_limits();
        log_socket_limits();
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(100), "100 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1048576), "1.0 MB");
        assert_eq!(format_bytes(1073741824), "1.0 GB");
    }

    #[tokio::test]
    async fn test_socket_with_small_buffers() {
        // This test verifies we can still create sockets even with restricted buffers
        use socket2::{Domain, Protocol, Socket, Type};

        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))
            .unwrap();

        // Try to set very small buffers
        let _ = socket.set_recv_buffer_size(8192); // 8KB
        let _ = socket.set_send_buffer_size(8192);

        // Should still work even with small buffers
        socket.bind(&addr.into()).unwrap();
        socket.set_nonblocking(true).unwrap();

        // Convert to tokio socket should work
        let std_socket: std::net::UdpSocket = socket.into();
        let _tokio_socket = UdpSocket::from_std(std_socket).unwrap();
    }
}