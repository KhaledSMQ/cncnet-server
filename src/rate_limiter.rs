use rater::{IpRateLimiterManager, RateLimiterConfig};
use std::net::IpAddr;
use std::sync::Arc;

/// Per-IP connection-attempt rate limiter backed by a token bucket.
///
/// This limits how fast a given IP can make new connection attempts,
/// **not** the number of concurrent connections. Tokens replenish
/// over time, so a legitimate client that reconnects occasionally
/// will never be blocked.
#[derive(Clone)]
pub struct ConnectionRateLimiter {
    manager: Arc<IpRateLimiterManager>,
}

impl ConnectionRateLimiter {
    pub fn new(max_per_ip: u32) -> Self {
        let config = RateLimiterConfig::new(max_per_ip as u64, max_per_ip, 60_000);

        let manager = Arc::new(IpRateLimiterManager::with_cleanup_settings(
            config, 60_000, 300_000,
        ));

        Self { manager }
    }

    /// Consume one token for `ip`. Returns `true` if under the limit.
    pub fn try_acquire(&self, ip: &IpAddr) -> bool {
        self.manager.try_acquire(*ip)
    }

    /// Number of distinct IPs currently tracked by the rate limiter.
    pub fn get_active_ip_count(&self) -> usize {
        self.manager.active_ips()
    }
}

/// Per-IP request throttle backed by a token bucket.
#[derive(Clone)]
pub struct RateLimiter {
    manager: Arc<IpRateLimiterManager>,
}

impl RateLimiter {
    pub fn new(window_seconds: u64, max_per_ip: u32) -> Self {
        let config = RateLimiterConfig::new(max_per_ip as u64, max_per_ip, window_seconds * 1000);

        let manager = Arc::new(IpRateLimiterManager::with_cleanup_settings(
            config,
            window_seconds * 1000,
            window_seconds * 5000,
        ));

        Self { manager }
    }

    /// Returns `true` if the request is allowed (token available).
    pub fn check_and_increment(&self, ip: &IpAddr) -> bool {
        self.manager.try_acquire(*ip)
    }

    /// Clear all tracked IPs, effectively resetting all rate limits.
    pub fn reset(&self) {
        self.manager.clear();
    }
}
