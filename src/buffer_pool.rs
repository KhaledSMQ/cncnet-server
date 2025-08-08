use bytes::{BytesMut, Bytes};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

static GLOBAL_POOL: OnceLock<BufferPool> = OnceLock::new();

pub struct BufferPool {
    small: Mutex<VecDeque<BytesMut>>,  // 64 bytes
    medium: Mutex<VecDeque<BytesMut>>, // 1024 bytes
    large: Mutex<VecDeque<BytesMut>>,  // 8192 bytes

    // Pool statistics for monitoring
    small_hits: AtomicUsize,
    small_misses: AtomicUsize,
    medium_hits: AtomicUsize,
    medium_misses: AtomicUsize,
    large_hits: AtomicUsize,
    large_misses: AtomicUsize,

    // Track allocations for leak detection
    small_allocated: AtomicUsize,
    medium_allocated: AtomicUsize,
    large_allocated: AtomicUsize,
}

#[derive(Debug, Clone)]
pub struct PoolStats {
    pub small_hit_rate: f64,
    pub medium_hit_rate: f64,
    pub large_hit_rate: f64,
    pub small_allocated: usize,
    pub medium_allocated: usize,
    pub large_allocated: usize,
    pub small_pooled: usize,
    pub medium_pooled: usize,
    pub large_pooled: usize,
}

impl BufferPool {
    fn new() -> Self {
        // Pre-allocate some buffers
        let mut small_pool = VecDeque::with_capacity(1000);
        let mut medium_pool = VecDeque::with_capacity(500);
        let mut large_pool = VecDeque::with_capacity(100);

        // Pre-fill pools for better startup performance
        for _ in 0..100 {
            let mut buf = BytesMut::with_capacity(64);
            buf.resize(64, 0);
            small_pool.push_back(buf);
        }

        for _ in 0..50 {
            let mut buf = BytesMut::with_capacity(1024);
            buf.resize(1024, 0);
            medium_pool.push_back(buf);
        }

        for _ in 0..10 {
            let mut buf = BytesMut::with_capacity(8192);
            buf.resize(8192, 0);
            large_pool.push_back(buf);
        }

        Self {
            small: Mutex::new(small_pool),
            medium: Mutex::new(medium_pool),
            large: Mutex::new(large_pool),
            small_hits: AtomicUsize::new(0),
            small_misses: AtomicUsize::new(0),
            medium_hits: AtomicUsize::new(0),
            medium_misses: AtomicUsize::new(0),
            large_hits: AtomicUsize::new(0),
            large_misses: AtomicUsize::new(0),
            small_allocated: AtomicUsize::new(100),
            medium_allocated: AtomicUsize::new(50),
            large_allocated: AtomicUsize::new(10),
        }
    }

    pub fn acquire_small(&self) -> BytesMut {
        let mut pool = self.small.lock();
        if let Some(mut buf) = pool.pop_front() {
            self.small_hits.fetch_add(1, Ordering::Relaxed);
            buf.clear();
            buf.resize(64, 0);
            buf
        } else {
            self.small_misses.fetch_add(1, Ordering::Relaxed);
            self.small_allocated.fetch_add(1, Ordering::Relaxed);
            let mut buf = BytesMut::with_capacity(64);
            buf.resize(64, 0);
            buf
        }
    }

    pub fn acquire_medium(&self) -> BytesMut {
        let mut pool = self.medium.lock();
        if let Some(mut buf) = pool.pop_front() {
            self.medium_hits.fetch_add(1, Ordering::Relaxed);
            buf.clear();
            buf.resize(1024, 0);
            buf
        } else {
            self.medium_misses.fetch_add(1, Ordering::Relaxed);
            self.medium_allocated.fetch_add(1, Ordering::Relaxed);
            let mut buf = BytesMut::with_capacity(1024);
            buf.resize(1024, 0);
            buf
        }
    }

    pub fn acquire_large(&self) -> BytesMut {
        let mut pool = self.large.lock();
        if let Some(mut buf) = pool.pop_front() {
            self.large_hits.fetch_add(1, Ordering::Relaxed);
            buf.clear();
            buf.resize(8192, 0);
            buf
        } else {
            self.large_misses.fetch_add(1, Ordering::Relaxed);
            self.large_allocated.fetch_add(1, Ordering::Relaxed);
            let mut buf = BytesMut::with_capacity(8192);
            buf.resize(8192, 0);
            buf
        }
    }

    pub fn release_small(&self, mut buf: BytesMut) {
        // Validate buffer size to prevent wrong pool usage
        if buf.capacity() >= 32 && buf.capacity() <= 128 {
            buf.clear();
            buf.resize(64, 0); // Reset to standard size

            let mut pool = self.small.lock();
            if pool.len() < 1000 {
                pool.push_back(buf);
            } else {
                // Pool is full, let buffer deallocate
                self.small_allocated.fetch_sub(1, Ordering::Relaxed);
            }
        }
        // Wrong size buffer, let it deallocate
    }

    pub fn release_medium(&self, mut buf: BytesMut) {
        // Validate buffer size to prevent wrong pool usage
        if buf.capacity() >= 512 && buf.capacity() <= 2048 {
            buf.clear();
            buf.resize(1024, 0); // Reset to standard size

            let mut pool = self.medium.lock();
            if pool.len() < 500 {
                pool.push_back(buf);
            } else {
                // Pool is full, let buffer deallocate
                self.medium_allocated.fetch_sub(1, Ordering::Relaxed);
            }
        }
        // Wrong size buffer, let it deallocate
    }

    pub fn release_large(&self, mut buf: BytesMut) {
        // Validate buffer size to prevent wrong pool usage
        if buf.capacity() >= 4096 && buf.capacity() <= 16384 {
            buf.clear();
            buf.resize(8192, 0); // Reset to standard size

            let mut pool = self.large.lock();
            if pool.len() < 100 {
                pool.push_back(buf);
            } else {
                // Pool is full, let buffer deallocate
                self.large_allocated.fetch_sub(1, Ordering::Relaxed);
            }
        }
        // Wrong size buffer, let it deallocate
    }

    pub fn get_stats(&self) -> PoolStats {
        let small_pool = self.small.lock();
        let medium_pool = self.medium.lock();
        let large_pool = self.large.lock();

        PoolStats {
            small_hit_rate: self.calculate_hit_rate(
                self.small_hits.load(Ordering::Relaxed),
                self.small_misses.load(Ordering::Relaxed),
            ),
            medium_hit_rate: self.calculate_hit_rate(
                self.medium_hits.load(Ordering::Relaxed),
                self.medium_misses.load(Ordering::Relaxed),
            ),
            large_hit_rate: self.calculate_hit_rate(
                self.large_hits.load(Ordering::Relaxed),
                self.large_misses.load(Ordering::Relaxed),
            ),
            small_allocated: self.small_allocated.load(Ordering::Relaxed),
            medium_allocated: self.medium_allocated.load(Ordering::Relaxed),
            large_allocated: self.large_allocated.load(Ordering::Relaxed),
            small_pooled: small_pool.len(),
            medium_pooled: medium_pool.len(),
            large_pooled: large_pool.len(),
        }
    }

    fn calculate_hit_rate(&self, hits: usize, misses: usize) -> f64 {
        let total = hits + misses;
        if total == 0 {
            0.0
        } else {
            (hits as f64 / total as f64) * 100.0
        }
    }

    pub fn clear_pools(&self) {
        // Clear all pools (useful for testing or shutdown)
        self.small.lock().clear();
        self.medium.lock().clear();
        self.large.lock().clear();
    }

    pub fn shrink_pools(&self) {
        // Shrink pools to 50% if they're too full (memory optimization)
        {
            let mut small = self.small.lock();
            if small.len() > 500 {
                small.truncate(500);
            }
        }
        {
            let mut medium = self.medium.lock();
            if medium.len() > 250 {
                medium.truncate(250);
            }
        }
        {
            let mut large = self.large.lock();
            if large.len() > 50 {
                large.truncate(50);
            }
        }
    }
}

pub fn init_global_pool() {
    GLOBAL_POOL.get_or_init(|| BufferPool::new());
    tracing::info!("Buffer pool initialized with pre-allocated buffers");
}

pub fn get_pool() -> &'static BufferPool {
    GLOBAL_POOL.get().expect("Buffer pool not initialized")
}

// RAII guard for automatic buffer release
pub struct BufferGuard {
    buf: Option<BytesMut>,
    size_class: SizeClass,
}

#[derive(Debug, Clone, Copy)]
enum SizeClass {
    Small,
    Medium,
    Large,
}

impl BufferGuard {
    pub fn small(buf: BytesMut) -> Self {
        Self {
            buf: Some(buf),
            size_class: SizeClass::Small,
        }
    }

    pub fn medium(buf: BytesMut) -> Self {
        Self {
            buf: Some(buf),
            size_class: SizeClass::Medium,
        }
    }

    pub fn large(buf: BytesMut) -> Self {
        Self {
            buf: Some(buf),
            size_class: SizeClass::Large,
        }
    }

    pub fn take(&mut self) -> Option<BytesMut> {
        self.buf.take()
    }

    pub fn as_ref(&self) -> Option<&BytesMut> {
        self.buf.as_ref()
    }

    pub fn as_mut(&mut self) -> Option<&mut BytesMut> {
        self.buf.as_mut()
    }
}

impl Drop for BufferGuard {
    fn drop(&mut self) {
        if let Some(buf) = self.buf.take() {
            let pool = get_pool();
            match self.size_class {
                SizeClass::Small => pool.release_small(buf),
                SizeClass::Medium => pool.release_medium(buf),
                SizeClass::Large => pool.release_large(buf),
            }
        }
    }
}

// Periodic maintenance task
pub async fn maintenance_task() {
    use tokio::time::{interval, Duration};

    let mut interval = interval(Duration::from_secs(60));

    loop {
        interval.tick().await;

        let pool = get_pool();
        let stats = pool.get_stats();

        // Shrink pools if hit rate is too high (meaning we have too many buffers)
        if stats.medium_hit_rate > 95.0 {
            pool.shrink_pools();
            tracing::debug!("Shrinking buffer pools due to high hit rate");
        }

        // Log statistics periodically
        tracing::debug!(
            "Buffer pool stats - Small: {:.1}% hit rate, Medium: {:.1}% hit rate, Large: {:.1}% hit rate",
            stats.small_hit_rate,
            stats.medium_hit_rate,
            stats.large_hit_rate
        );
    }
}