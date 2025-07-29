//! Peer-to-peer NAT traversal service
//!
//! This module implements a STUN-like functionality for NAT traversal,
//! allowing clients to discover their external IP addresses and ports.
//!
//! The service is designed with a focus on performance, security, low memory usage, and error resilience.
//! Key improvements over the original implementation:
//! - Configurable limits for rate limiting to allow tuning in production.
//! - Use of `IpAddr` as HashMap keys for accurate per-IP tracking without hash collisions.
//! - Atomic global request counter to prevent DoS via spoofed IP flooding.
//! - Optimized locking: Uses rate limiter with atomic operations for lock-free performance.
//! - Explicit IPv4-only support (binds to IPv4, rejects IPv6 requests early).
//! - Pre-allocated response buffer pool to avoid allocations in hot path.
//! - Enhanced logging for rate limit triggers, errors, and key events.
//! - Graceful shutdown support via a cancellation token.
//! - Comprehensive error handling with timeouts on all operations.
//! - Low memory: Bounded capacity with automatic cleanup.
//! - Security: Prevents spoofing amplification by rate limiting; no amplification factor.
//! - Backpressure support to prevent system overload.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;
use tokio::net::UdpSocket;
use tokio::sync::{oneshot::{self, Receiver}, Semaphore};
use tokio::time::{self, timeout};
use tracing::{debug, error, info, warn};
use socket2::{Domain, Protocol, Socket, Type};
use crate::net::constants::STUN_ID;
use crate::net::rate_limiter::{IpRateLimiterManager, RateLimiter, RateLimiterConfig as RLConfig, MemoryOrdering};

/// Maximum concurrent packet processing tasks
const MAX_CONCURRENT_PROCESSING: usize = 1000;

/// Timeout for UDP operations
const UDP_OPERATION_TIMEOUT: Duration = Duration::from_millis(100);

/// Maximum packet size we'll accept
const MAX_PACKET_SIZE: usize = 64;

/// Required packet size for valid requests
const REQUIRED_PACKET_SIZE: usize = 48;

/// Response packet size
const RESPONSE_SIZE: usize = 40;

/// XOR mask for address obfuscation
const XOR_MASK: u8 = 0x20;

/// Default counter reset interval in milliseconds.
const DEFAULT_COUNTER_RESET_INTERVAL_MS: u64 = 60_000;

/// Default maximum requests per IP during one interval.
const DEFAULT_MAX_REQUESTS_PER_IP: usize = 20;

/// Default maximum total requests (global) during one interval.
const DEFAULT_MAX_REQUESTS_GLOBAL: usize = 5000;

/// Configuration for the Peer-to-Peer service.
#[derive(Debug, Clone)]
pub struct PeerToPeerConfig {
    /// Listen port for the service.
    pub port: u16,
    /// Maximum requests per IP during one reset interval.
    pub max_requests_per_ip: usize,
    /// Maximum total requests globally during one reset interval.
    pub max_requests_global: usize,
    /// Interval for resetting counters (milliseconds).
    pub reset_interval_ms: u64,
    /// Maximum concurrent packet processing tasks
    pub max_concurrent_processing: usize,
}

impl Default for PeerToPeerConfig {
    fn default() -> Self {
        Self {
            port: 8054, // Default CnCNet port.
            max_requests_per_ip: DEFAULT_MAX_REQUESTS_PER_IP,
            max_requests_global: DEFAULT_MAX_REQUESTS_GLOBAL,
            reset_interval_ms: DEFAULT_COUNTER_RESET_INTERVAL_MS,
            max_concurrent_processing: MAX_CONCURRENT_PROCESSING,
        }
    }
}

/// Response buffer pool to avoid allocations
struct ResponseBufferPool {
    /// Pre-generated base response buffers
    buffers: Vec<[u8; RESPONSE_SIZE]>,
    /// Current index for round-robin
    index: AtomicUsize,
}

impl ResponseBufferPool {
    /// Creates a new buffer pool with pre-initialized buffers
    fn new(size: usize) -> Self {
        let mut buffers = Vec::with_capacity(size);

        for _ in 0..size {
            let mut buffer = [0u8; RESPONSE_SIZE];

            // Initialize with random data for protocol noise
            use rand::RngCore;
            rand::rng().fill_bytes(&mut buffer);

            // Set fixed STUN_ID at offset 6 (big-endian)
            let stun_bytes = STUN_ID.to_be_bytes();
            buffer[6] = stun_bytes[0];
            buffer[7] = stun_bytes[1];

            buffers.push(buffer);
        }

        Self {
            buffers,
            index: AtomicUsize::new(0),
        }
    }

    /// Gets a response buffer (round-robin from pool)
    pub fn get_buffer(&self) -> [u8; RESPONSE_SIZE] {
        let idx = self.index.fetch_add(1, Ordering::Relaxed) % self.buffers.len();
        self.buffers[idx]
    }
}

/// Peer-to-peer service for NAT traversal.
pub struct PeerToPeerService {
    /// Service configuration.
    config: PeerToPeerConfig,

    /// Manager for per-IP rate limiting.
    per_ip_manager: Arc<IpRateLimiterManager>,

    /// Global rate limiter.
    global_limiter: Arc<RateLimiter>,

    /// Response buffer pool
    buffer_pool: Arc<ResponseBufferPool>,

    /// Semaphore for backpressure control
    processing_semaphore: Arc<Semaphore>,

    /// Metrics
    metrics: Arc<ServiceMetrics>,
}

/// Service metrics for monitoring
struct ServiceMetrics {
    /// Total packets received
    packets_received: AtomicU64,
    /// Total packets processed successfully
    packets_processed: AtomicU64,
    /// Total packets dropped due to rate limits
    packets_rate_limited: AtomicU64,
    /// Total invalid packets
    packets_invalid: AtomicU64,
    /// Total processing errors
    processing_errors: AtomicU64,
}

impl ServiceMetrics {
    fn new() -> Self {
        Self {
            packets_received: AtomicU64::new(0),
            packets_processed: AtomicU64::new(0),
            packets_rate_limited: AtomicU64::new(0),
            packets_invalid: AtomicU64::new(0),
            processing_errors: AtomicU64::new(0),
        }
    }
}

impl PeerToPeerService {
    /// Creates a new peer-to-peer service with default configuration.
    pub fn new(port: u16) -> Self {
        Self::with_config(PeerToPeerConfig {
            port,
            ..Default::default()
        })
    }

    /// Creates a new peer-to-peer service with custom configuration.
    pub fn with_config(config: PeerToPeerConfig) -> Self {
        // Configure global rate limiter
        let global_rl_config = RLConfig {
            max_tokens: config.max_requests_global as u32,
            refill_rate: config.max_requests_global as u32,
            refill_interval_ms: config.reset_interval_ms,
            ordering: MemoryOrdering::Relaxed,
        };
        let global_limiter = Arc::new(RateLimiter::with_config(global_rl_config));

        // Configure per-IP rate limiter manager
        let per_ip_rl_config = RLConfig {
            max_tokens: config.max_requests_per_ip as u32,
            refill_rate: config.max_requests_per_ip as u32,
            refill_interval_ms: config.reset_interval_ms,
            ordering: MemoryOrdering::Relaxed,
        };
        let per_ip_manager = Arc::new(IpRateLimiterManager::new(per_ip_rl_config));

        // Create buffer pool (size based on expected concurrency)
        let buffer_pool = Arc::new(ResponseBufferPool::new(
            config.max_concurrent_processing.min(1000)
        ));

        // Create processing semaphore for backpressure
        let processing_semaphore = Arc::new(Semaphore::new(config.max_concurrent_processing));

        Self {
            config,
            per_ip_manager,
            global_limiter,
            buffer_pool,
            processing_semaphore,
            metrics: Arc::new(ServiceMetrics::new()),
        }
    }

    /// Starts the peer-to-peer service and runs until the cancellation receiver is triggered.
    ///
    /// This version configures the socket completely before integrating with Tokio.
    pub async fn start(
        self,
    ) -> Result<(oneshot::Sender<()>, tokio::task::JoinHandle<Result<()>>)> {
        info!(
        "Starting peer-to-peer service on port {} with config: {:?}",
        self.config.port, self.config
    );

        let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

        sock.set_recv_buffer_size(256 * 1024)?;
        sock.set_send_buffer_size(128 * 1024)?;
        // You could set other options here too, e.g., `sock.set_reuse_address(true)?`.

        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), self.config.port);
        sock.bind(&addr.into())?;

        sock.set_nonblocking(true)?;
        debug!("Socket created and configured successfully.");

        let tokio_socket = UdpSocket::from_std(sock.into())?;
        let socket = Arc::new(tokio_socket);

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let handle = tokio::spawn(self.run(socket, shutdown_rx));

        Ok((shutdown_tx, handle))
    }


    /// Internal run loop for the service.
    async fn run(self, socket: Arc<UdpSocket>, mut shutdown_rx: Receiver<()>) -> Result<()> {
        // Start per-IP cleanup task
        let cleanup_handle = self.per_ip_manager.clone().start_cleanup_task();

        // Start metrics reporting task
        let metrics_handle = {
            let metrics = self.metrics.clone();
            tokio::spawn(async move {
                let mut interval = time::interval(Duration::from_secs(60));
                interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

                loop {
                    interval.tick().await;
                    let received = metrics.packets_received.load(Ordering::Relaxed);
                    let processed = metrics.packets_processed.load(Ordering::Relaxed);
                    let rate_limited = metrics.packets_rate_limited.load(Ordering::Relaxed);
                    let invalid = metrics.packets_invalid.load(Ordering::Relaxed);
                    let errors = metrics.processing_errors.load(Ordering::Relaxed);

                    info!(
                        "P2P metrics - received: {}, processed: {}, rate_limited: {}, invalid: {}, errors: {}",
                        received, processed, rate_limited, invalid, errors
                    );
                }
            })
        };

        // Pre-allocate receive buffer
        let mut recv_buf = vec![0u8; MAX_PACKET_SIZE];

        // Main receive loop
        loop {
            tokio::select! {
                // Handle shutdown signal
                _ = &mut shutdown_rx => {
                    info!("P2P service shutting down");
                    cleanup_handle.abort();
                    metrics_handle.abort();
                    break Ok(());
                }

                // Receive UDP packets with timeout
                result = timeout(UDP_OPERATION_TIMEOUT, socket.recv_from(&mut recv_buf)) => {
                    match result {
                        Ok(Ok((size, addr))) => {
                            self.metrics.packets_received.fetch_add(1, Ordering::Relaxed);

                            // Quick validation before spawning task
                            if size != REQUIRED_PACKET_SIZE {
                                self.metrics.packets_invalid.fetch_add(1, Ordering::Relaxed);
                                debug!("Ignored invalid packet size {} from {}", size, addr);
                                continue;
                            }

                            // Try to acquire processing permit (backpressure)
                            let permit = match self.processing_semaphore.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    // System overloaded, drop packet
                                    warn!("P2P processing queue full, dropping packet from {}", addr);
                                    continue;
                                }
                            };

                            // Clone data for async processing
                            let data = recv_buf[..size].to_vec();
                            let socket = socket.clone();
                            let service = self.clone();

                            // Spawn task to handle request
                            tokio::spawn(async move {
                                let _permit = permit; // Hold permit until done
                                service.handle_request(&data, addr, &socket).await;
                            });
                        }
                        Ok(Err(e)) => {
                            error!("P2P UDP receive error: {}", e);
                            self.metrics.processing_errors.fetch_add(1, Ordering::Relaxed);
                            // Continue on error to maintain resilience
                        }
                        Err(_) => {
                            // Timeout - this is normal, continue
                        }
                    }
                }
            }
        }
    }

    /// Handles a received request packet.
    async fn handle_request(&self, data: &[u8], remote_addr: SocketAddr, socket: &UdpSocket) {
        // Validate source address (prevent local/invalid sources)
        if !self.is_valid_source_address(&remote_addr) {
            self.metrics.packets_invalid.fetch_add(1, Ordering::Relaxed);
            debug!("Ignored invalid source address: {}", remote_addr);
            return;
        }

        // Early reject IPv6 (protocol is IPv4-only)
        let ip = remote_addr.ip();
        if ip.is_ipv6() {
            self.metrics.packets_invalid.fetch_add(1, Ordering::Relaxed);
            debug!("Ignored IPv6 request from {}", remote_addr);
            return;
        }

        // Check rate limits (global first for quick rejection)
        if !self.check_rate_limits(ip) {
            self.metrics.packets_rate_limited.fetch_add(1, Ordering::Relaxed);
            debug!("Rate limit exceeded for {}", remote_addr);
            return;
        }

        // Validate STUN_ID in request (bytes 0-1, big-endian i16)
        let received_id = i16::from_be_bytes([data[0], data[1]]);
        if received_id != STUN_ID {
            self.metrics.packets_invalid.fetch_add(1, Ordering::Relaxed);
            debug!("Invalid STUN_ID from {}", remote_addr);
            return;
        }

        // Extract IPv4 bytes (safe after is_ipv6 check)
        let ip_bytes = match ip {
            IpAddr::V4(v4) => v4.octets(),
            _ => unreachable!(),
        };

        // Get response buffer from pool
        let mut send_buffer = self.buffer_pool.get_buffer();

        // Insert IP and port into buffer
        send_buffer[0] = ip_bytes[0];
        send_buffer[1] = ip_bytes[1];
        send_buffer[2] = ip_bytes[2];
        send_buffer[3] = ip_bytes[3];
        let port_bytes = remote_addr.port().to_be_bytes();
        send_buffer[4] = port_bytes[0];
        send_buffer[5] = port_bytes[1];

        // Obfuscate address data (XOR with 0x20, per protocol)
        for i in 0..6 {
            send_buffer[i] ^= XOR_MASK;
        }

        // Send response with timeout
        match timeout(UDP_OPERATION_TIMEOUT, socket.send_to(&send_buffer, remote_addr)).await {
            Ok(Ok(_)) => {
                self.metrics.packets_processed.fetch_add(1, Ordering::Relaxed);
                debug!("Sent P2P response to {}", remote_addr);
            }
            Ok(Err(e)) => {
                self.metrics.processing_errors.fetch_add(1, Ordering::Relaxed);
                error!("Failed to send P2P response to {}: {}", remote_addr, e);
            }
            Err(_) => {
                self.metrics.processing_errors.fetch_add(1, Ordering::Relaxed);
                error!("Timeout sending P2P response to {}", remote_addr);
            }
        }
    }

    /// Validates that the source address is acceptable
    #[inline]
    pub fn is_valid_source_address(&self, addr: &SocketAddr) -> bool {
        !addr.ip().is_loopback()
            && !addr.ip().is_unspecified()
            && addr.port() != 0
    }

    /// Checks if rate limit is exceeded for the given IP
    /// Returns true if allowed, false if rate limited
    #[inline]
    pub fn check_rate_limits(&self, ip: IpAddr) -> bool {
        // Global limit first (fail fast)
        if !self.global_limiter.try_acquire() {
            return false;
        }

        // Per-IP limit
        self.per_ip_manager.try_acquire(ip)
    }

    /// Gets current service metrics
    pub fn metrics(&self) -> P2PMetrics {
        P2PMetrics {
            packets_received: self.metrics.packets_received.load(Ordering::Relaxed),
            packets_processed: self.metrics.packets_processed.load(Ordering::Relaxed),
            packets_rate_limited: self.metrics.packets_rate_limited.load(Ordering::Relaxed),
            packets_invalid: self.metrics.packets_invalid.load(Ordering::Relaxed),
            processing_errors: self.metrics.processing_errors.load(Ordering::Relaxed),
            active_ips: self.per_ip_manager.active_ips(),
        }
    }
}

/// Clone implementation for PeerToPeerService
impl Clone for PeerToPeerService {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            per_ip_manager: self.per_ip_manager.clone(),
            global_limiter: self.global_limiter.clone(),
            buffer_pool: self.buffer_pool.clone(),
            processing_semaphore: self.processing_semaphore.clone(),
            metrics: self.metrics.clone(),
        }
    }
}

/// Public metrics structure
#[derive(Debug, Clone)]
pub struct P2PMetrics {
    pub packets_received: u64,
    pub packets_processed: u64,
    pub packets_rate_limited: u64,
    pub packets_invalid: u64,
    pub processing_errors: u64,
    pub active_ips: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[test]
    fn test_stun_id_encoding() {
        let service = PeerToPeerService::new(8054);
        let buffer = service.buffer_pool.get_buffer();

        // Check that STUN_ID is correctly encoded in buffer
        let stun_bytes = STUN_ID.to_be_bytes();
        assert_eq!(buffer[6], stun_bytes[0]);
        assert_eq!(buffer[7], stun_bytes[1]);
    }

    #[test]
    fn test_buffer_pool() {
        let pool = ResponseBufferPool::new(10);

        // Get multiple buffers
        let buf1 = pool.get_buffer();
        let buf2 = pool.get_buffer();

        // Should have STUN_ID set
        let stun_bytes = STUN_ID.to_be_bytes();
        assert_eq!(buf1[6], stun_bytes[0]);
        assert_eq!(buf1[7], stun_bytes[1]);
        assert_eq!(buf2[6], stun_bytes[0]);
        assert_eq!(buf2[7], stun_bytes[1]);
    }

    #[tokio::test]
    async fn test_valid_source_address() {
        let service = PeerToPeerService::new(8054);

        // Valid addresses
        let valid_addr: SocketAddr = "192.168.1.1:1234".parse().unwrap();
        assert!(service.is_valid_source_address(&valid_addr));

        // Invalid addresses
        let loopback: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        assert!(!service.is_valid_source_address(&loopback));

        let unspecified: SocketAddr = "0.0.0.0:1234".parse().unwrap();
        assert!(!service.is_valid_source_address(&unspecified));

        let zero_port: SocketAddr = "192.168.1.1:0".parse().unwrap();
        assert!(!service.is_valid_source_address(&zero_port));
    }

    #[tokio::test]
    async fn test_shutdown() {
        let service = PeerToPeerService::new(0); // Port 0 for auto-bind
        let (shutdown_tx, handle) = service.start().await.unwrap();

        // Give time to start
        sleep(Duration::from_millis(100)).await;

        // Signal shutdown
        shutdown_tx.send(()).unwrap();

        // Wait for task to complete
        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_rate_limiting() {
        let mut config = PeerToPeerConfig::default();
        config.max_requests_per_ip = 5;
        let service = PeerToPeerService::with_config(config);

        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        // Should allow up to max_requests_per_ip
        for _ in 0..5 {
            assert!(service.check_rate_limits(ip));
        }

        // Should be rate limited
        assert!(!service.check_rate_limits(ip));
    }

    #[tokio::test]
    async fn test_metrics() {
        let service = PeerToPeerService::new(0);

        // Initial metrics should be zero
        let metrics = service.metrics();
        assert_eq!(metrics.packets_received, 0);
        assert_eq!(metrics.packets_processed, 0);

        // Simulate some activity
        service.metrics.packets_received.fetch_add(10, Ordering::Relaxed);
        service.metrics.packets_processed.fetch_add(8, Ordering::Relaxed);
        service.metrics.packets_rate_limited.fetch_add(2, Ordering::Relaxed);

        let metrics = service.metrics();
        assert_eq!(metrics.packets_received, 10);
        assert_eq!(metrics.packets_processed, 8);
        assert_eq!(metrics.packets_rate_limited, 2);
    }
}