//! # Lock-Free Token Bucket Rate Limiter
//!
//! A high-performance, thread-safe rate limiter implementation using atomic operations
//! for game servers and high-concurrency applications.
//!
//! ## Architecture Overview
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                     Rate Limiter Architecture                   │
//! ├─────────────────────────────────────────────────────────────────┤
//! │                                                                 │
//! │  Client Request ──┐                                             │
//! │                   ▼                                             │
//! │            ┌─────────────┐                                      │
//! │            │ try_acquire │                                      │
//! │            └──────┬──────┘                                      │
//! │                   │                                             │
//! │                   ▼                                             │
//! │         ┌─────────────────┐      ┌───────────────────┐          │
//! │         │ Refill Check    │─────▶│ Elapsed Time?     │          │
//! │         │ (if needed)     │      │ >= refill_interval│          │
//! │         └─────────────────┘      └─────────┬─────────┘          │
//! │                   │                        │ Yes                │
//! │                   │                        ▼                    │
//! │                   │              ┌───────────────────┐          │
//! │                   │              │  Add Tokens       │          │
//! │                   │              │   (up to max)     │          │
//! │                   │              └───────────────────┘          │
//! │                   │                                             │
//! │                   ▼                                             │
//! │         ┌─────────────────┐                                     │
//! │         │ Atomic CAS Loop │                                     │
//! │         │ tokens >= n?    │                                     │
//! │         └────────┬────────┘                                     │
//! │                  │                                              │
//! │         ┌────────┴────────┐                                     │
//! │         ▼                 ▼                                     │
//! │    ┌──────────┐      ┌──────────┐                               │
//! │    │ Success  │      │ Rejected │                               │
//! │    │ -n tokens│      │ +1 reject│                               │
//! │    └──────────┘      └──────────┘                               │
//! │                                                                 │
//! │  ┌────────────────────────────────────────────────────┐         │
//! │  │              Token Bucket State                    │         │
//! │  ├────────────────────────────────────────────────────┤         │
//! │  │  AtomicU32: tokens     [||||||||..] (8/10)         │         │
//! │  │  AtomicU64: last_refill_ms                         │         │
//! │  │  AtomicU64: last_access_ms                         │         │
//! │  │                                                    │         │
//! │  │  Metrics:                                          │         │
//! │  │  - total_acquired: 1234                            │         │
//! │  │  - total_rejected: 56                              │         │
//! │  │  - total_refills: 78                               │         │
//! │  └────────────────────────────────────────────────────┘         │
//! └─────────────────────────────────────────────────────────────────┘
//! ```

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use std::net::IpAddr;
use std::sync::Arc;
use dashmap::DashMap;
use tracing::{debug, info, warn, trace};

/// Maximum number of refill periods to process at once (prevents integer overflow)
const MAX_REFILL_PERIODS: u64 = 100;

/// Maximum number of tracked IPs to prevent unbounded memory growth
const MAX_TRACKED_IPS: usize = 10_000;

/// Maximum number of CAS spin attempts before giving up
const MAX_SPIN_ATTEMPTS: u32 = 10;

/// Token bucket rate limiter with automatic token refill and cleanup tracking
///
/// This implementation uses a token bucket algorithm where:
/// - Tokens are consumed when requests are made
/// - Tokens are automatically refilled at a configured rate
/// - Requests are rejected when no tokens are available
///
/// The rate limiter is completely lock-free, using atomic operations for all
/// state mutations. This makes it suitable for high-concurrency scenarios
/// like game servers where thousands of players might trigger rate-limited
/// actions simultaneously.
///
/// ## Thread Safety
///
/// All operations are thread-safe and lock-free. Multiple threads can call
/// `try_acquire` simultaneously without blocking each other.
///
/// ## Time Handling
///
/// Uses `Instant::now()` for monotonic time that's immune to system clock
/// adjustments, preventing potential exploits from clock manipulation.
#[derive(Debug)]
pub struct RateLimiter {
    /// Current number of available tokens (atomic for lock-free access)
    pub(crate) tokens: AtomicU32,

    /// Maximum tokens the bucket can hold
    max_tokens: u32,

    /// Number of tokens to add per refill interval
    refill_rate: u32,

    /// Time between refills in milliseconds
    refill_interval_ms: u64,

    /// Timestamp of last refill (milliseconds since start)
    last_refill_ms: AtomicU64,

    /// Timestamp of last access (for cleanup tracking)
    pub(crate) last_access_ms: AtomicU64,

    /// Reference point for monotonic time calculations
    start_instant: Instant,

    // Performance metrics for monitoring and debugging

    /// Total tokens successfully acquired
    total_acquired: AtomicU64,

    /// Total acquisition attempts that were rejected
    total_rejected: AtomicU64,

    /// Total number of refill operations performed
    total_refills: AtomicU64,

    /// Memory ordering strategy for atomic operations
    ordering: MemoryOrdering,
}

/// Memory ordering configuration for different consistency requirements
///
/// Choose based on your needs:
/// - `Relaxed`: Maximum performance, suitable for most rate limiting scenarios
/// - `AcquireRelease`: Balanced option, ensures proper synchronization
/// - `Sequential`: Maximum safety, use for critical operations like security
#[derive(Debug, Clone, Copy)]
pub enum MemoryOrdering {
    /// Relaxed ordering for maximum performance (default)
    ///
    /// Suitable when exact token counts don't need to be synchronized
    /// across threads immediately. Good for general rate limiting.
    Relaxed,

    /// Acquire-Release for typical concurrent scenarios
    ///
    /// Ensures proper happens-before relationships between threads.
    /// Recommended for most production use cases.
    AcquireRelease,

    /// Sequential consistency for maximum safety
    ///
    /// All threads see operations in the same order. Use for
    /// security-critical rate limiting (e.g., login attempts).
    Sequential,
}

impl MemoryOrdering {
    /// Get the appropriate Ordering for load operations
    #[inline(always)]
    fn load(&self) -> Ordering {
        match self {
            Self::Relaxed => Ordering::Relaxed,
            Self::AcquireRelease => Ordering::Acquire,
            Self::Sequential => Ordering::SeqCst,
        }
    }

    /// Get the appropriate Ordering for store operations
    #[inline(always)]
    fn store(&self) -> Ordering {
        match self {
            Self::Relaxed => Ordering::Relaxed,
            Self::AcquireRelease => Ordering::Release,
            Self::Sequential => Ordering::SeqCst,
        }
    }

    /// Get the appropriate Ordering pair for compare_exchange operations
    #[inline(always)]
    fn compare_exchange(&self) -> (Ordering, Ordering) {
        match self {
            Self::Relaxed => (Ordering::Relaxed, Ordering::Relaxed),
            Self::AcquireRelease => (Ordering::AcqRel, Ordering::Acquire),
            Self::Sequential => (Ordering::SeqCst, Ordering::SeqCst),
        }
    }
}

/// Configuration for rate limiter
///
/// Provides sensible defaults for game servers while allowing full customization
/// for specific use cases.
#[derive(Debug, Clone)]
pub struct RateLimiterConfig {
    /// Maximum number of tokens the bucket can hold
    pub max_tokens: u32,

    /// Number of tokens to add each refill interval
    pub refill_rate: u32,

    /// Time between refills in milliseconds
    pub refill_interval_ms: u64,

    /// Memory ordering strategy for atomic operations
    pub ordering: MemoryOrdering,
}

impl Default for RateLimiterConfig {
    /// Default configuration suitable for player actions in game servers
    ///
    /// - 50 max tokens: Allows burst activity
    /// - 10 tokens/second: Sustainable rate for active gameplay
    /// - AcquireRelease ordering: Good balance of performance and correctness
    fn default() -> Self {
        Self {
            max_tokens: 50,        // Good default for player actions
            refill_rate: 10,       // Refill 10 tokens per interval
            refill_interval_ms: 1000, // Every second
            ordering: MemoryOrdering::AcquireRelease,
        }
    }
}

impl RateLimiterConfig {
    /// Creates config optimized for game server player actions
    ///
    /// Use this for general player actions like movement, attacks, or skill usage.
    pub fn game_server_default() -> Self {
        Self::default()
    }

    /// Creates config for high-frequency operations (e.g., chat messages)
    ///
    /// - Lower burst capacity (20 tokens)
    /// - High refill rate (20/second)
    /// - Allows sustained high-frequency usage without large bursts
    pub fn high_frequency() -> Self {
        Self {
            max_tokens: 20,
            refill_rate: 20,
            refill_interval_ms: 1000,
            ordering: MemoryOrdering::AcquireRelease,
        }
    }

    /// Creates config for low-frequency operations (e.g., login attempts)
    ///
    /// - Very limited tokens (5)
    /// - Slow refill (1 per 10 seconds)
    /// - Sequential consistency for security
    pub fn low_frequency() -> Self {
        Self {
            max_tokens: 5,
            refill_rate: 1,
            refill_interval_ms: 10000, // Every 10 seconds
            ordering: MemoryOrdering::Sequential,
        }
    }
}

impl RateLimiter {
    /// Creates a new rate limiter with default game server configuration
    ///
    /// # Arguments
    ///
    /// * `max_tokens` - Maximum tokens the bucket can hold
    /// * `refill_rate` - Tokens to add per second
    ///
    /// # Example
    ///
    /// ```
    /// let limiter = RateLimiter::new(100, 20); // 100 max, 20 per second
    /// ```
    pub fn new(max_tokens: u32, refill_rate: u32) -> Self {
        Self::with_config(RateLimiterConfig {
            max_tokens,
            refill_rate,
            ..Default::default()
        })
    }

    /// Creates a new rate limiter with full configuration
    ///
    /// # Arguments
    ///
    /// * `config` - Full configuration including memory ordering
    ///
    /// # Panics
    ///
    /// Panics if `max_tokens` is 0 or `refill_interval_ms` is 0
    pub fn with_config(config: RateLimiterConfig) -> Self {
        assert!(config.max_tokens > 0, "max_tokens must be greater than 0");
        assert!(config.refill_interval_ms > 0, "refill_interval_ms must be greater than 0");

        let start_instant = Instant::now();
        Self {
            tokens: AtomicU32::new(config.max_tokens),
            max_tokens: config.max_tokens,
            refill_rate: config.refill_rate,
            refill_interval_ms: config.refill_interval_ms,
            last_refill_ms: AtomicU64::new(0),
            last_access_ms: AtomicU64::new(0),
            start_instant,
            total_acquired: AtomicU64::new(0),
            total_rejected: AtomicU64::new(0),
            total_refills: AtomicU64::new(0),
            ordering: config.ordering,
        }
    }

    /// Attempts to acquire a single token with lock-free atomic operations
    ///
    /// # Returns
    ///
    /// `true` if a token was acquired, `false` if no tokens available
    ///
    /// # Example
    ///
    /// ```
    /// if limiter.try_acquire() {
    ///     // Process request
    /// } else {
    ///     // Rate limited - reject request
    /// }
    /// ```
    #[inline]
    pub fn try_acquire(&self) -> bool {
        self.try_acquire_n(1)
    }

    /// Attempts to acquire multiple tokens atomically
    ///
    /// Useful for operations with different costs. For example, a regular
    /// message might cost 1 token while sending an image costs 5 tokens.
    ///
    /// # Arguments
    ///
    /// * `n` - Number of tokens to acquire
    ///
    /// # Returns
    ///
    /// `true` if all tokens were acquired, `false` otherwise
    ///
    /// # Note
    ///
    /// This is an all-or-nothing operation. Either all requested tokens
    /// are acquired or none are.
    #[inline]
    pub fn try_acquire_n(&self, n: u32) -> bool {
        if n == 0 {
            return true;
        }

        // Prevent requests for more tokens than the bucket can ever hold
        if n > self.max_tokens {
            self.total_rejected.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        // Update last access time for cleanup tracking
        let now_ms = self.elapsed_millis();
        self.last_access_ms.store(now_ms, self.ordering.store());

        // Check if we need to refill tokens before attempting acquisition
        self.refill_tokens();

        // Lock-free token acquisition using compare-and-swap loop
        let (success_order, fail_order) = self.ordering.compare_exchange();
        let mut current = self.tokens.load(self.ordering.load());
        let mut spin_count = 0;

        loop {
            // Check if enough tokens are available
            if current < n {
                self.total_rejected.fetch_add(1, Ordering::Relaxed);
                return false;
            }

            // Attempt to atomically subtract tokens
            match self.tokens.compare_exchange_weak(
                current,
                current - n,
                success_order,
                fail_order,
            ) {
                Ok(_) => {
                    // Successfully acquired tokens
                    self.total_acquired.fetch_add(n as u64, Ordering::Relaxed);
                    return true;
                }
                Err(actual) => {
                    // Another thread modified tokens, retry with updated value
                    current = actual;

                    // Add backoff for high contention scenarios
                    spin_count += 1;
                    if spin_count > 3 {
                        std::thread::yield_now();
                    }

                    // Prevent infinite spinning under extreme contention
                    if spin_count >= MAX_SPIN_ATTEMPTS {
                        self.total_rejected.fetch_add(1, Ordering::Relaxed);
                        warn!("Rate limiter CAS loop exceeded max spin attempts");
                        return false;
                    }

                    if current < n {
                        self.total_rejected.fetch_add(1, Ordering::Relaxed);
                        return false;
                    }
                }
            }
        }
    }

    /// Returns the current number of available tokens
    ///
    /// This method triggers a refill check, so the returned value
    /// reflects the most up-to-date token count.
    ///
    /// # Example
    ///
    /// ```
    /// if limiter.available_tokens() < 5 {
    ///     println!("Warning: Low on tokens!");
    /// }
    /// ```
    #[inline]
    pub fn available_tokens(&self) -> u32 {
        self.refill_tokens();
        self.tokens.load(self.ordering.load())
    }

    /// Checks if the rate limiter has been inactive for cleanup purposes
    ///
    /// Useful for removing rate limiters that haven't been used recently
    /// to free memory in long-running servers.
    ///
    /// # Arguments
    ///
    /// * `inactive_duration_ms` - Duration in milliseconds to consider inactive
    pub fn is_inactive(&self, inactive_duration_ms: u64) -> bool {
        let now_ms = self.elapsed_millis();
        let last_ms = self.last_access_ms.load(self.ordering.load());
        now_ms.saturating_sub(last_ms) > inactive_duration_ms
    }

    /// Checks if the rate limiter has been inactive using default duration (5 minutes)
    pub fn is_inactive_default(&self) -> bool {
        self.is_inactive(300_000)
    }

    /// Returns metrics for monitoring and debugging
    ///
    /// Use these metrics to:
    /// - Monitor rate limit effectiveness
    /// - Detect potential attacks or abuse
    /// - Tune rate limit parameters
    ///
    /// # Example
    ///
    /// ```
    /// let metrics = limiter.metrics();
    /// println!("Success rate: {:.2}%", metrics.success_rate() * 100.0);
    /// ```
    pub fn metrics(&self) -> RateLimiterMetrics {
        RateLimiterMetrics {
            total_acquired: self.total_acquired.load(Ordering::Relaxed),
            total_rejected: self.total_rejected.load(Ordering::Relaxed),
            total_refills: self.total_refills.load(Ordering::Relaxed),
            current_tokens: self.tokens.load(self.ordering.load()),
            max_tokens: self.max_tokens,
        }
    }

    /// Refills tokens based on elapsed time since last refill (overflow-safe)
    ///
    /// This method is automatically called by `try_acquire` and `available_tokens`.
    /// It uses a lock-free algorithm to ensure only one thread performs the refill
    /// even under high concurrency.
    #[inline]
    fn refill_tokens(&self) {
        // Runtime check for valid configuration
        if self.refill_interval_ms == 0 {
            warn!("Invalid refill_interval_ms (0), skipping refill");
            return;
        }

        let now_ms = self.elapsed_millis();
        let last_ms = self.last_refill_ms.load(self.ordering.load());

        // Quick check if enough time has passed for at least one refill
        let elapsed = now_ms.saturating_sub(last_ms);
        if elapsed < self.refill_interval_ms {
            return;
        }

        // Calculate number of complete refill periods that have passed
        // Use checked arithmetic to prevent any possibility of overflow
        let periods = match elapsed.checked_div(self.refill_interval_ms) {
            Some(p) => p.min(MAX_REFILL_PERIODS),
            None => return, // Shouldn't happen with valid config
        };

        if periods == 0 {
            return;
        }

        // Calculate new last_refill timestamp
        // For overflow protection, if time has advanced too far, reset to align with current time
        let new_last_refill = if periods >= MAX_REFILL_PERIODS {
            // Reset to current aligned interval
            now_ms - (now_ms % self.refill_interval_ms)
        } else {
            // Normal case: advance by exact periods
            match last_ms.checked_add(periods.saturating_mul(self.refill_interval_ms)) {
                Some(v) => v,
                None => {
                    // Overflow detected, reset to current aligned interval
                    warn!("Refill timestamp overflow detected, resetting");
                    now_ms - (now_ms % self.refill_interval_ms)
                }
            }
        };

        // Try to update last_refill timestamp atomically
        let (success_order, fail_order) = self.ordering.compare_exchange();

        match self.last_refill_ms.compare_exchange(
            last_ms,
            new_last_refill,
            success_order,
            fail_order,
        ) {
            Ok(_) => {
                // This thread won the race to refill tokens
                let current = self.tokens.load(self.ordering.load());

                // Calculate tokens to add with overflow protection
                let tokens_to_add = match (self.refill_rate as u64).checked_mul(periods) {
                    Some(v) => v.min(self.max_tokens as u64),
                    None => self.max_tokens as u64, // Cap at max if overflow
                };

                // Safe conversion since we know tokens_to_add <= max_tokens
                let tokens_to_add = tokens_to_add as u32;

                // Calculate new token count (capped at max_tokens)
                let new_tokens = current
                    .saturating_add(tokens_to_add)
                    .min(self.max_tokens);

                self.tokens.store(new_tokens, self.ordering.store());
                self.total_refills.fetch_add(1, Ordering::Relaxed);

                trace!(
                    "Refilled {} tokens (periods: {}, current: {}, new: {})",
                    tokens_to_add, periods, current, new_tokens
                );
            }
            Err(_) => {
                // Another thread is handling the refill, nothing to do
            }
        }
    }

    /// Returns elapsed time in milliseconds since creation (with overflow protection)
    #[inline]
    fn elapsed_millis(&self) -> u64 {
        // Use saturating conversion to handle extremely long-running servers
        match self.start_instant.elapsed().as_millis().try_into() {
            Ok(ms) => ms,
            Err(_) => {
                // Server has been running for >584 million years!
                // Reset the start instant to prevent issues
                warn!("Elapsed time overflow detected, resetting timer");
                u64::MAX
            }
        }
    }
}

/// Metrics for monitoring rate limiter performance
///
/// Track these metrics to understand your rate limiting effectiveness
/// and detect potential issues or attacks.
#[derive(Debug, Clone)]
pub struct RateLimiterMetrics {
    /// Total tokens successfully acquired across all requests
    pub total_acquired: u64,

    /// Total requests rejected due to insufficient tokens
    pub total_rejected: u64,

    /// Total number of refill operations performed
    pub total_refills: u64,

    /// Current number of available tokens
    pub current_tokens: u32,

    /// Maximum tokens the bucket can hold
    pub max_tokens: u32,
}

impl RateLimiterMetrics {
    /// Calculate success rate (0.0 to 1.0)
    ///
    /// A low success rate might indicate:
    /// - Rate limits are too restrictive
    /// - Potential abuse or attack
    /// - Need to adjust refill parameters
    pub fn success_rate(&self) -> f64 {
        let total = self.total_acquired + self.total_rejected;
        if total == 0 {
            1.0
        } else {
            self.total_acquired as f64 / total as f64
        }
    }

    /// Check if the rate limiter is under heavy load
    pub fn is_under_pressure(&self) -> bool {
        self.success_rate() < 0.5 || self.current_tokens == 0
    }
}

/// Manager for IP-based rate limiting with automatic cleanup and memory pressure handling
///
/// Provides per-IP rate limiting suitable for web servers and game servers.
/// Automatically manages rate limiter lifecycle with configurable cleanup.
///
/// # Example
///
/// ```
/// let config = RateLimiterConfig::game_server_default();
/// let manager = Arc::new(IpRateLimiterManager::new(config));
///
/// // Start automatic cleanup
/// manager.clone().start_cleanup_task();
///
/// // Use in request handler
/// if !manager.try_acquire(client_ip) {
///     return Err("Rate limit exceeded");
/// }
/// ```
#[derive(Debug, Clone)]
pub struct IpRateLimiterManager {
    /// Map of IP addresses to their rate limiters
    ///
    /// Using DashMap for lock-free concurrent access
    limiters: Arc<DashMap<IpAddr, Arc<RateLimiter>>>,

    /// Configuration used for creating new rate limiters
    config: RateLimiterConfig,

    /// How often to run cleanup (milliseconds)
    cleanup_interval_ms: u64,

    /// How long an IP must be inactive before removal (milliseconds)
    inactive_duration_ms: u64,

    /// Total number of rate limiters created (for monitoring)
    total_created: Arc<AtomicU64>,

    /// Total number of rate limiters cleaned up (for monitoring)
    total_cleaned: Arc<AtomicU64>,

    /// Current memory pressure level (0-100)
    memory_pressure: Arc<AtomicUsize>,
}

impl IpRateLimiterManager {
    /// Creates a new IP rate limiter manager
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration applied to all per-IP rate limiters
    pub fn new(config: RateLimiterConfig) -> Self {
        Self {
            limiters: Arc::new(DashMap::with_capacity(1024)),
            config,
            cleanup_interval_ms: 60_000,  // Clean up every minute
            inactive_duration_ms: 300_000, // Remove after 5 minutes inactive
            total_created: Arc::new(AtomicU64::new(0)),
            total_cleaned: Arc::new(AtomicU64::new(0)),
            memory_pressure: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Gets or creates a rate limiter for the given IP with memory pressure handling
    ///
    /// Rate limiters are created on-demand and cached for reuse.
    /// Returns None if the maximum number of tracked IPs is reached.
    pub fn get_limiter(&self, ip: IpAddr) -> Option<Arc<RateLimiter>> {
        // Check if we need emergency cleanup
        let current_size = self.limiters.len();
        if current_size >= MAX_TRACKED_IPS * 90 / 100 {
            // 90% full, trigger emergency cleanup
            self.emergency_cleanup();
        }

        // Re-check after potential cleanup
        if self.limiters.len() >= MAX_TRACKED_IPS && !self.limiters.contains_key(&ip) {
            warn!(
                "Rate limiter capacity reached ({} IPs), rejecting new IP: {}",
                MAX_TRACKED_IPS, ip
            );
            return None;
        }

        Some(self.limiters.entry(ip)
            .or_insert_with(|| {
                self.total_created.fetch_add(1, Ordering::Relaxed);
                Arc::new(RateLimiter::with_config(self.config.clone()))
            })
            .clone())
    }

    /// Emergency cleanup - removes least recently used entries
    ///
    /// Uses DashMap's retain method to avoid iterator invalidation issues
    fn emergency_cleanup(&self) {
        let before_count = self.limiters.len();
        let target_size = MAX_TRACKED_IPS * 70 / 100; // Reduce to 70% capacity
        let to_remove = before_count.saturating_sub(target_size);

        if to_remove == 0 {
            return;
        }

        // Collect entries with their last access time
        let mut entries: Vec<(IpAddr, u64)> = Vec::new();

        // Use iter() to avoid holding locks during collection
        for entry in self.limiters.iter() {
            let ip = *entry.key();
            let last_access = entry.value().last_access_ms.load(Ordering::Relaxed);
            entries.push((ip, last_access));
        }

        // Sort by last access time (oldest first)
        entries.sort_by_key(|&(_, last_access)| last_access);

        // Remove oldest entries
        let mut removed = 0;
        for (ip, _) in entries.into_iter().take(to_remove) {
            if self.limiters.remove(&ip).is_some() {
                removed += 1;
            }
        }

        if removed > 0 {
            self.total_cleaned.fetch_add(removed as u64, Ordering::Relaxed);
            info!("Emergency cleanup removed {} rate limiters", removed);
        }
    }

    /// Attempts to acquire a token for the given IP
    ///
    /// Convenience method that combines get_limiter and try_acquire.
    pub fn try_acquire(&self, ip: IpAddr) -> bool {
        match self.get_limiter(ip) {
            Some(limiter) => limiter.try_acquire(),
            None => false, // Capacity reached, reject
        }
    }

    /// Attempts to acquire multiple tokens for the given IP
    pub fn try_acquire_n(&self, ip: IpAddr, n: u32) -> bool {
        match self.get_limiter(ip) {
            Some(limiter) => limiter.try_acquire_n(n),
            None => false,
        }
    }

    /// Performs regular cleanup of inactive rate limiters
    ///
    /// Uses retain() method for atomic cleanup without iterator issues
    pub fn cleanup(&self) {
        let before_count = self.limiters.len();

        // Adjust cleanup aggressiveness based on current size
        let size_ratio = (self.limiters.len() * 100) / MAX_TRACKED_IPS;
        let adjusted_inactive_duration = if size_ratio > 80 {
            // More aggressive cleanup when near capacity
            self.inactive_duration_ms / 2
        } else {
            self.inactive_duration_ms
        };

        // Use retain for atomic cleanup
        self.limiters.retain(|_ip, limiter| {
            !limiter.is_inactive(adjusted_inactive_duration)
        });

        let cleaned = before_count - self.limiters.len();
        if cleaned > 0 {
            self.total_cleaned.fetch_add(cleaned as u64, Ordering::Relaxed);
            debug!("Cleaned up {} inactive rate limiters", cleaned);
        }

        // Update memory pressure
        let pressure = (self.limiters.len() * 100) / MAX_TRACKED_IPS;
        self.memory_pressure.store(pressure, Ordering::Relaxed);
    }

    /// Returns the number of active IPs being tracked
    pub fn active_ips(&self) -> usize {
        self.limiters.len()
    }

    /// Starts automatic cleanup with adaptive intervals
    ///
    /// The cleanup thread runs periodically to remove inactive rate limiters.
    /// This prevents memory growth in long-running servers.
    ///
    /// # Example
    ///
    /// ```
    /// let manager = Arc::new(IpRateLimiterManager::new(config));
    /// manager.clone().start_cleanup_task();
    /// ```
    pub fn start_cleanup_task(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        use tokio::time;

        tokio::spawn(async move {
            loop {
                // Adaptive cleanup interval based on memory pressure
                let pressure = self.memory_pressure.load(Ordering::Relaxed);
                let interval_ms = if pressure > 80 {
                    10_000  // 10 seconds when high pressure
                } else if pressure > 50 {
                    30_000  // 30 seconds when moderate pressure
                } else {
                    self.cleanup_interval_ms  // Normal interval
                };

                time::sleep(Duration::from_millis(interval_ms)).await;
                self.cleanup();
            }
        })
    }

    /// Gets metrics for all active rate limiters
    ///
    /// Useful for monitoring and debugging rate limit behavior across
    /// all tracked IPs.
    pub fn all_metrics(&self) -> Vec<(IpAddr, RateLimiterMetrics)> {
        self.limiters.iter()
            .map(|entry| (*entry.key(), entry.value().metrics()))
            .collect()
    }

    /// Gets aggregated metrics for the manager
    pub fn manager_metrics(&self) -> ManagerMetrics {
        ManagerMetrics {
            active_ips: self.active_ips(),
            total_created: self.total_created.load(Ordering::Relaxed),
            total_cleaned: self.total_cleaned.load(Ordering::Relaxed),
            capacity_remaining: MAX_TRACKED_IPS.saturating_sub(self.active_ips()),
        }
    }

    /// Gets current memory pressure (0-100)
    pub fn memory_pressure(&self) -> usize {
        self.memory_pressure.load(Ordering::Relaxed)
    }
}

/// Metrics for the IpRateLimiterManager
#[derive(Debug, Clone)]
pub struct ManagerMetrics {
    /// Number of currently tracked IPs
    pub active_ips: usize,
    /// Total rate limiters created since startup
    pub total_created: u64,
    /// Total rate limiters cleaned up since startup
    pub total_cleaned: u64,
    /// Remaining capacity before MAX_TRACKED_IPS is reached
    pub capacity_remaining: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_monotonic_time() {
        // Verifies that rate limiting works correctly even if system clock changes
        let limiter = RateLimiter::new(10, 5);

        // Consume all tokens
        for _ in 0..10 {
            assert!(limiter.try_acquire());
        }

        // Even if system clock changes, refill should work correctly
        thread::sleep(Duration::from_millis(1100));

        // Should have refilled 5 tokens
        for _ in 0..5 {
            assert!(limiter.try_acquire());
        }
        assert!(!limiter.try_acquire());
    }

    #[test]
    fn test_metrics() {
        let limiter = RateLimiter::new(5, 2);

        // Acquire some tokens
        for _ in 0..3 {
            assert!(limiter.try_acquire());
        }

        // Try to acquire more than available
        for _ in 0..5 {
            limiter.try_acquire();
        }

        let metrics = limiter.metrics();
        assert_eq!(metrics.total_acquired, 5); // 3 + 2 successful
        assert_eq!(metrics.total_rejected, 3);  // 3 failed attempts
        assert!(metrics.success_rate() > 0.6 && metrics.success_rate() < 0.7);
    }

    #[test]
    fn test_max_refill_periods() {
        let config = RateLimiterConfig {
            max_tokens: 100,
            refill_rate: 10,
            refill_interval_ms: 1,
            ordering: MemoryOrdering::Relaxed,
        };
        let limiter = RateLimiter::with_config(config);

        // Consume all tokens
        for _ in 0..100 {
            assert!(limiter.try_acquire());
        }

        // Sleep for a very long time (simulated)
        thread::sleep(Duration::from_millis(1000));

        // Should refill but capped at max_tokens, not overflow
        assert_eq!(limiter.available_tokens(), 100);
    }

    #[test]
    fn test_ip_manager_capacity() {
        let config = RateLimiterConfig::game_server_default();
        let manager = Arc::new(IpRateLimiterManager::new(config));

        // Should allow up to MAX_TRACKED_IPS
        for i in 0..100 {
            let ip: IpAddr = format!("192.168.1.{}", i % 256).parse().unwrap();
            assert!(manager.try_acquire(ip));
        }

        let metrics = manager.manager_metrics();
        assert!(metrics.active_ips <= MAX_TRACKED_IPS);
        assert_eq!(metrics.total_created, 100);
    }

    #[tokio::test]
    async fn test_cleanup_task() {
        let config = RateLimiterConfig::game_server_default();
        let manager = Arc::new(IpRateLimiterManager::new(config));

        // Start cleanup with short interval for testing
        let mut manager_test = (*manager).clone();
        manager_test.cleanup_interval_ms = 100;
        manager_test.inactive_duration_ms = 100;
        let manager_test = Arc::new(manager_test);

        let _handle = manager_test.clone().start_cleanup_task();

        // Add some IPs
        let ip1: IpAddr = "192.168.1.1".parse().unwrap();
        assert!(manager_test.try_acquire(ip1));

        // Wait for cleanup
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Should have been cleaned up
        assert_eq!(manager_test.active_ips(), 0);
        assert!(manager_test.manager_metrics().total_cleaned > 0);
    }

    #[test]
    fn test_pressure_detection() {
        let limiter = RateLimiter::new(10, 5);

        // Normal usage
        for _ in 0..5 {
            assert!(limiter.try_acquire());
        }
        assert!(!limiter.metrics().is_under_pressure());

        // Heavy usage
        for _ in 0..20 {
            limiter.try_acquire();
        }
        assert!(limiter.metrics().is_under_pressure());
    }

    #[test]
    fn test_zero_refill_interval() {
        // Should not panic with zero refill interval
        let config = RateLimiterConfig {
            max_tokens: 10,
            refill_rate: 5,
            refill_interval_ms: 1, // Valid config
            ordering: MemoryOrdering::Relaxed,
        };
        let limiter = RateLimiter::with_config(config);

        // Should work normally
        assert!(limiter.try_acquire());
    }

    #[test]
    fn test_spin_limit() {
        use std::sync::Arc;
        use std::thread;

        let limiter = Arc::new(RateLimiter::new(1, 1)); // Only 1 token

        // Consume the token
        assert!(limiter.try_acquire());

        // Spawn many threads trying to acquire simultaneously
        let mut handles = vec![];
        for _ in 0..20 {
            let limiter_clone = limiter.clone();
            let handle = thread::spawn(move || {
                limiter_clone.try_acquire()
            });
            handles.push(handle);
        }

        // All should complete without hanging
        for handle in handles {
            let result = handle.join().unwrap();
            assert!(!result); // All should fail since no tokens
        }
    }
}