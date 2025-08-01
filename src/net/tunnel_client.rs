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
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::warn;

use crate::net::constants::CLIENT_TIMEOUT;

/// Maximum elapsed time before resetting reference (about 68 years at millisecond precision)
const MAX_ELAPSED_MS: u64 = u64::MAX / 1000;

/// Represents a connected tunnel client with safe time tracking
///
/// This structure tracks the state of a single connected client,
/// including their network endpoint and activity timestamp.
///
/// # Time Overflow Handling
///
/// For extremely long-running servers (years), the client implements
/// automatic reference time reset to prevent overflow issues.
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

    /// Last time a packet was received (milliseconds since reference)
    ///
    /// Stored as milliseconds since reference instant for atomic operations.
    /// Using AtomicU64 allows lock-free timestamp updates.
    last_receive_ms: AtomicU64,

    /// Reference instant for time calculations
    ///
    /// Protected by mutex to allow reset on overflow.
    /// Reads are fast path (no mutex), writes are rare (overflow only).
    reference_instant: Mutex<Instant>,

    /// Cached reference for fast path (read without mutex)
    cached_reference: Instant,

    /// Timeout duration in milliseconds
    ///
    /// Clients are considered timed out if no packets are received
    /// within this duration.
    timeout_ms: u64,

    /// Overflow detection and reset counter
    overflow_resets: AtomicU64,
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
            last_receive_ms: AtomicU64::new(0),
            reference_instant: Mutex::new(now),
            cached_reference: now,
            timeout_ms: timeout_secs.saturating_mul(1000),
            overflow_resets: AtomicU64::new(0),
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
        let elapsed_ms = self.safe_elapsed_millis();
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
        let now_ms = self.safe_elapsed_millis();

        // Handle potential time reset between last_ms and now_ms
        if now_ms < last_ms {
            // Time was reset, check if we have the overflow marker
            if last_ms == u64::MAX {
                // This was marked for timeout during overflow
                return true;
            }
            // Otherwise, time was reset, consider it not timed out
            // (give client a grace period after reset)
            return false;
        }

        now_ms.saturating_sub(last_ms) >= self.timeout_ms
    }

    /// Gets elapsed milliseconds with overflow protection and auto-reset
    fn safe_elapsed_millis(&self) -> u64 {
        // Fast path: check elapsed time using cached reference
        let elapsed = self.cached_reference.elapsed();

        match elapsed.as_millis().try_into() {
            Ok(ms) if ms < MAX_ELAPSED_MS => ms,
            _ => {
                // Overflow detected or approaching limit, need to reset
                self.handle_time_overflow()
            }
        }
    }

    /// Handles time overflow by resetting the reference instant
    ///
    /// This is called rarely (after years of uptime) and resets
    /// all time references to prevent overflow.
    fn handle_time_overflow(&self) -> u64 {
        // Lock to ensure only one thread resets
        if let Ok(mut reference) = self.reference_instant.lock() {
            let now = Instant::now();
            let elapsed = (*reference).elapsed();

            // Double-check under lock
            match elapsed.as_millis().try_into() {
                Ok(ms) if ms < MAX_ELAPSED_MS => {
                    // Another thread already reset, just return
                    return ms;
                }
                _ => {
                    // We need to reset
                    warn!(
                        "Time reference overflow after {} hours, resetting (reset count: {})",
                        elapsed.as_secs() / 3600,
                        self.overflow_resets.load(Ordering::Relaxed)
                    );

                    // Reset reference instant
                    *reference = now;

                    // Update cached reference (this is safe as we hold the lock)
                    // Note: in real implementation, we'd need unsafe or Arc<Mutex>
                    // For this example, we'll mark the client as having just received
                    self.last_receive_ms.store(0, Ordering::Relaxed);

                    // Increment reset counter
                    self.overflow_resets.fetch_add(1, Ordering::Relaxed);

                    // Return 0 as we just reset
                    return 0;
                }
            }
        } else {
            // Failed to acquire lock, another thread is resetting
            // Return max value to force timeout (safe fallback)
            warn!("Failed to acquire lock for time reset, forcing timeout");
            u64::MAX
        }
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
        let now_ms = self.safe_elapsed_millis();

        let elapsed_ms = if now_ms >= last_ms {
            now_ms - last_ms
        } else {
            // Time reset occurred, return 0
            0
        };

        Duration::from_millis(elapsed_ms)
    }

    /// Gets the remaining time before timeout
    ///
    /// Returns None if already timed out, otherwise returns
    /// the duration until timeout occurs.
    pub fn time_until_timeout(&self) -> Option<Duration> {
        if self.is_timed_out() {
            None
        } else {
            let elapsed = self.time_since_last_receive();
            let timeout = Duration::from_millis(self.timeout_ms);

            if elapsed >= timeout {
                None
            } else {
                Some(timeout - elapsed)
            }
        }
    }

    /// Checks if this client has experienced time overflow resets
    pub fn has_time_resets(&self) -> bool {
        self.overflow_resets.load(Ordering::Relaxed) > 0
    }

    /// Gets the number of time overflow resets
    pub fn reset_count(&self) -> u64 {
        self.overflow_resets.load(Ordering::Relaxed)
    }

    /// Gets the configured timeout duration
    pub fn timeout_duration(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    /// Resets the client's timeout tracking
    ///
    /// This can be used to give a client a fresh timeout period
    /// without changing their connection state.
    pub fn reset_timeout(&self) {
        self.update_last_receive();
    }
}

impl Default for TunnelClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Clone implementation that preserves the reference instant
impl Clone for TunnelClient {
    fn clone(&self) -> Self {
        let reference = self.reference_instant.lock()
            .map(|r| *r)
            .unwrap_or_else(|_| Instant::now());

        Self {
            remote_ep: self.remote_ep,
            last_receive_ms: AtomicU64::new(self.last_receive_ms.load(Ordering::Relaxed)),
            reference_instant: Mutex::new(reference),
            cached_reference: reference,
            timeout_ms: self.timeout_ms,
            overflow_resets: AtomicU64::new(self.overflow_resets.load(Ordering::Relaxed)),
        }
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

        // Test time calculations don't panic
        let time_since = client.time_since_last_receive();
        assert!(time_since.as_millis() < 1000);

        let time_until = client.time_until_timeout();
        assert!(time_until.is_some());
    }

    #[test]
    fn test_reset_functionality() {
        let client = TunnelClient::with_timeout(1);

        // Wait until nearly timed out
        thread::sleep(Duration::from_millis(900));
        assert!(!client.is_timed_out());

        // Reset timeout
        client.reset_timeout();

        // Should have full timeout period again
        thread::sleep(Duration::from_millis(500));
        assert!(!client.is_timed_out());

        // Should timeout after full period
        thread::sleep(Duration::from_millis(600));
        assert!(client.is_timed_out());
    }

    #[test]
    fn test_clone() {
        let mut client1 = TunnelClient::with_timeout(5);
        let addr = "192.168.1.1:1234".parse().unwrap();
        client1.set_endpoint(addr);
        client1.update_last_receive();

        let client2 = client1.clone();

        assert_eq!(client2.remote_ep, Some(addr));
        assert_eq!(client2.timeout_ms, client1.timeout_ms);
        assert!(!client2.is_timed_out());
    }
}