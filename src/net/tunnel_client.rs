//! Tunnel client representation
//!
//! This module defines the client structure used by tunnel servers
//! to track connected clients and their timeout status.
//!
//! ## Design
//!
//! The `TunnelClient` structure tracks:
//! - Remote endpoint information
//! - Last activity timestamp
//! - Timeout configuration
//!
//! ## Thread Safety
//!
//! The client uses atomic operations for timestamp updates, allowing
//! safe concurrent access from multiple threads without locks.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::net::constants::CLIENT_TIMEOUT;

/// Represents a connected tunnel client
///
/// This structure tracks the state of a single connected client,
/// including their network endpoint and activity timestamp.
///
/// # Example
///
/// ```rust
/// let mut client = TunnelClient::new();
/// client.set_endpoint("192.168.1.100:5000".parse().unwrap());
///
/// // Activity tracking
/// client.update_last_receive();
/// assert!(!client.is_timed_out());
/// ```
#[derive(Debug)]
pub struct TunnelClient {
    /// Remote endpoint of the client
    ///
    /// This is the UDP address from which the client sends packets.
    /// It may change if the client's NAT mapping changes.
    pub remote_ep: Option<SocketAddr>,

    /// Last time a packet was received from this client
    ///
    /// Stored as milliseconds since creation for atomic operations.
    /// Using AtomicU64 allows lock-free timestamp updates.
    last_receive_ms: AtomicU64,

    /// Reference instant for time calculations
    ///
    /// This is set when the client is created and used as the
    /// reference point for all timeout calculations.
    created_at: Instant,

    /// Timeout duration in milliseconds
    ///
    /// Clients are considered timed out if no packets are received
    /// within this duration.
    timeout_ms: u64,
}

impl TunnelClient {
    /// Creates a new tunnel client with default timeout
    ///
    /// Uses the default timeout from constants (30 seconds).
    ///
    /// # Example
    ///
    /// ```rust
    /// let client = TunnelClient::new();
    /// assert!(!client.is_timed_out());
    /// ```
    pub fn new() -> Self {
        Self::with_timeout(CLIENT_TIMEOUT)
    }

    /// Creates a new tunnel client with specified timeout in seconds
    ///
    /// # Arguments
    ///
    /// * `timeout_secs` - Timeout duration in seconds
    ///
    /// # Example
    ///
    /// ```rust
    /// // Create client with 60 second timeout
    /// let client = TunnelClient::with_timeout(60);
    /// ```
    pub fn with_timeout(timeout_secs: u64) -> Self {
        let now = Instant::now();
        Self {
            remote_ep: None,
            last_receive_ms: AtomicU64::new(0), // Initialize to "now"
            created_at: now,
            timeout_ms: timeout_secs * 1000, // Convert to milliseconds
        }
    }

    /// Updates the last receive timestamp to current time
    ///
    /// This should be called whenever a valid packet is received
    /// from the client to keep the connection alive.
    ///
    /// # Thread Safety
    ///
    /// This method is thread-safe and can be called concurrently
    /// from multiple threads.
    pub fn update_last_receive(&self) {
        let elapsed_ms = self.created_at.elapsed().as_millis() as u64;
        self.last_receive_ms.store(elapsed_ms, Ordering::Relaxed);
    }

    /// Checks if the client has timed out
    ///
    /// Returns true if no packets have been received within the
    /// configured timeout duration.
    ///
    /// # Thread Safety
    ///
    /// This method is thread-safe and provides a consistent view
    /// of the timeout status.
    pub fn is_timed_out(&self) -> bool {
        let last_ms = self.last_receive_ms.load(Ordering::Relaxed);
        let now_ms = self.created_at.elapsed().as_millis() as u64;

        now_ms.saturating_sub(last_ms) >= self.timeout_ms
    }

    /// Sets the remote endpoint for this client
    ///
    /// # Arguments
    ///
    /// * `addr` - The socket address of the client
    ///
    /// # Example
    ///
    /// ```rust
    /// let mut client = TunnelClient::new();
    /// client.set_endpoint("192.168.1.100:5000".parse().unwrap());
    /// assert!(client.remote_ep.is_some());
    /// ```
    pub fn set_endpoint(&mut self, addr: SocketAddr) {
        self.remote_ep = Some(addr);
    }

    /// Gets the time since last packet was received
    ///
    /// Returns the duration since the last packet was received
    /// from this client. Useful for debugging and monitoring.
    pub fn time_since_last_receive(&self) -> Duration {
        let last_ms = self.last_receive_ms.load(Ordering::Relaxed);
        let now_ms = self.created_at.elapsed().as_millis() as u64;

        Duration::from_millis(now_ms.saturating_sub(last_ms))
    }

    /// Gets the remaining time before timeout
    ///
    /// Returns None if already timed out, otherwise returns
    /// the duration until timeout occurs.
    pub fn time_until_timeout(&self) -> Option<Duration> {
        let elapsed = self.time_since_last_receive();
        let timeout = Duration::from_millis(self.timeout_ms);

        if elapsed >= timeout {
            None
        } else {
            Some(timeout - elapsed)
        }
    }
}

impl Default for TunnelClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_client_timeout() {
        // Create client with 1 second timeout
        let client = TunnelClient::with_timeout(1);

        // Should not be timed out initially
        assert!(!client.is_timed_out());

        // Should have full timeout remaining
        let remaining = client.time_until_timeout().unwrap();
        assert!(remaining.as_millis() > 900); // Allow some margin

        // Wait for timeout
        thread::sleep(Duration::from_millis(1100));

        // Should now be timed out
        assert!(client.is_timed_out());
        assert!(client.time_until_timeout().is_none());

        // Update timestamp
        client.update_last_receive();

        // Should no longer be timed out
        assert!(!client.is_timed_out());
        assert!(client.time_until_timeout().is_some());
    }

    #[test]
    fn test_client_endpoint() {
        let mut client = TunnelClient::new();
        assert!(client.remote_ep.is_none());

        let addr = "127.0.0.1:8080".parse().unwrap();
        client.set_endpoint(addr);
        assert_eq!(client.remote_ep, Some(addr));
    }

    #[test]
    fn test_time_tracking() {
        let client = TunnelClient::with_timeout(10);

        // Initial time since last receive should be very small
        let initial_time = client.time_since_last_receive();
        assert!(initial_time.as_millis() < 100);

        // Wait a bit
        thread::sleep(Duration::from_millis(500));

        // Time should have increased
        let elapsed = client.time_since_last_receive();
        assert!(elapsed.as_millis() >= 500);
        assert!(elapsed.as_millis() < 600); // Allow some margin

        // Update receive time
        client.update_last_receive();

        // Time should reset
        let after_update = client.time_since_last_receive();
        assert!(after_update.as_millis() < 100);
    }

    #[test]
    fn test_concurrent_updates() {
        use std::sync::Arc;

        let client = Arc::new(TunnelClient::with_timeout(5));
        let mut handles = vec![];

        // Spawn multiple threads updating the client
        for _ in 0..10 {
            let client_clone = client.clone();
            let handle = thread::spawn(move || {
                for _ in 0..100 {
                    client_clone.update_last_receive();
                    thread::sleep(Duration::from_micros(100));
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Client should not be timed out
        assert!(!client.is_timed_out());
    }

    #[test]
    fn test_edge_cases() {
        // Test with very short timeout
        let client = TunnelClient::with_timeout(0);
        thread::sleep(Duration::from_millis(10));
        assert!(client.is_timed_out());

        // Test with very long timeout
        let client = TunnelClient::with_timeout(u64::MAX / 1000);
        assert!(!client.is_timed_out());

        // Test time calculations don't overflow
        let time_since = client.time_since_last_receive();
        assert!(time_since.as_millis() < 1000);

        let time_until = client.time_until_timeout();
        assert!(time_until.is_some());
    }
}
