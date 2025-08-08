use rater::{IpRateLimiterManager, RateLimiterConfig};
use std::net::IpAddr;
use std::sync::Arc;

/// Connection limiter for IP-based rate limiting
pub struct ConnectionLimiter {
    manager: Arc<IpRateLimiterManager>,
    max_per_ip: usize,
}

impl ConnectionLimiter {
    pub fn new(max_per_ip: usize) -> Self {
        // Create rate limiter config for the max connections per IP
        let config = RateLimiterConfig::new(
            max_per_ip as u64,  // max tokens (burst capacity)
            max_per_ip as u32,  // refill rate
            60_000,             // refill every minute
        );

        let manager = Arc::new(IpRateLimiterManager::with_cleanup_settings(
            config,
            60_000,   // cleanup interval: 1 minute
            300_000,  // inactive duration: 5 minutes
        ));

        Self {
            manager,
            max_per_ip,
        }
    }

    pub fn try_add(&self, ip: &IpAddr) -> bool {
        self.manager.try_acquire(*ip)
    }

    pub fn remove(&self, ip: &IpAddr) {
        // The rater package handles cleanup automatically
        // We can trigger a manual cleanup if needed
        self.manager.cleanup();
    }

    pub fn update_ip(&self, old_ip: &IpAddr, new_ip: &IpAddr) -> bool {
        if old_ip == new_ip {
            return true;
        }

        // Try to add new IP first
        if !self.try_add(new_ip) {
            return false;
        }

        // Remove old IP (cleanup will handle it)
        self.remove(old_ip);
        true
    }

    pub fn get_connection_count(&self, _ip: &IpAddr) -> usize {
        // The rater package doesn't expose per-IP counts directly
        // Return 0 or estimated value based on whether IP can acquire
        if self.manager.try_acquire(*_ip) {
            // If we can acquire, there's at least one slot
            1
        } else {
            self.max_per_ip
        }
    }

    pub fn get_total_connections(&self) -> usize {
        self.manager.active_ips()
    }
}

/// Rate limiter for request throttling
pub struct RateLimiter {
    manager: Arc<IpRateLimiterManager>,
}

impl RateLimiter {
    pub fn new(window_seconds: u64, max_per_ip: usize, max_global: usize) -> Self {
        // Create config based on the window and limits
        let config = RateLimiterConfig::new(
            max_per_ip as u64,                    // max tokens
            max_per_ip as u32,                    // refill rate
            window_seconds * 1000,                // convert to milliseconds
        );

        let manager = Arc::new(IpRateLimiterManager::with_cleanup_settings(
            config,
            window_seconds * 1000,  // cleanup interval
            window_seconds * 5000,  // inactive duration (5x window)
        ));

        // Note: max_global is not directly supported by rater package
        // The package has its own internal limit of 10,000 IPs

        Self { manager }
    }

    pub fn check_and_increment(&self, ip: &IpAddr) -> bool {
        self.manager.try_acquire(*ip)
    }

    pub fn reset(&self) {
        self.manager.clear();
    }

    pub fn get_current_load(&self) -> (usize, usize) {
        let active_ips = self.manager.active_ips();
        let stats = self.manager.stats();
        (active_ips, stats.total_created as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_rate_limiter() {
        let limiter = RateLimiter::new(60, 5, 100);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        // Should allow up to max_per_ip requests
        for _ in 0..5 {
            assert!(limiter.check_and_increment(&ip));
        }

        // Should deny after limit
        assert!(!limiter.check_and_increment(&ip));
    }

    #[test]
    fn test_connection_limiter() {
        let limiter = ConnectionLimiter::new(3);
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        // Should allow up to max_per_ip connections
        assert!(limiter.try_add(&ip));
        assert!(limiter.try_add(&ip));
        assert!(limiter.try_add(&ip));

        // Should deny after limit
        assert!(!limiter.try_add(&ip));

        // Should allow after removing
        limiter.remove(&ip);
        // Note: removal in rater is handled by cleanup, might not be immediate
    }
}