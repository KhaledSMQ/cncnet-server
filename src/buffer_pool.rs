use bytes::{BytesMut, Bytes};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

static GLOBAL_POOL: OnceLock<BufferPool> = OnceLock::new();

// ARM cache line size is 64 bytes
#[cfg(target_arch = "aarch64")]
#[repr(align(64))]
pub struct BufferPool {
    small: Mutex<VecDeque<BytesMut>>,  // 64 bytes
    medium: Mutex<VecDeque<BytesMut>>, // 1024 bytes
    large: Mutex<VecDeque<BytesMut>>,  // 8192 bytes

    // Pool statistics for monitoring - aligned for ARM
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

#[cfg(not(target_arch = "aarch64"))]
pub struct BufferPool {
    small: Mutex<VecDeque<BytesMut>>,
    medium: Mutex<VecDeque<BytesMut>>,
    large: Mutex<VecDeque<BytesMut>>,
    small_hits: AtomicUsize,
    small_misses: AtomicUsize,
    medium_hits: AtomicUsize,
    medium_misses: AtomicUsize,
    large_hits: AtomicUsize,
    large_misses: AtomicUsize,
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
        // ARM-optimized pool sizes
        #[cfg(target_arch = "aarch64")]
        let (small_cap, medium_cap, large_cap) = (2000, 1000, 200);

        #[cfg(not(target_arch = "aarch64"))]
        let (small_cap, medium_cap, large_cap) = (1000, 500, 100);

        let mut small_pool = VecDeque::with_capacity(small_cap);
        let mut medium_pool = VecDeque::with_capacity(medium_cap);
        let mut large_pool = VecDeque::with_capacity(large_cap);

        // Pre-fill pools - more aggressive on ARM due to better memory bandwidth
        #[cfg(target_arch = "aarch64")]
        {
            for _ in 0..200 {
                small_pool.push_back(Self::create_zeroed_buffer(64));
            }
            for _ in 0..100 {
                medium_pool.push_back(Self::create_zeroed_buffer(1024));
            }
            for _ in 0..20 {
                large_pool.push_back(Self::create_zeroed_buffer(8192));
            }
        }

        #[cfg(not(target_arch = "aarch64"))]
        {
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
            small_allocated: AtomicUsize::new(200),
            medium_allocated: AtomicUsize::new(100),
            large_allocated: AtomicUsize::new(20),
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn create_zeroed_buffer(size: usize) -> BytesMut {
        let mut buf = BytesMut::with_capacity(size);
        buf.resize(size, 0);
        Self::clear_buffer_neon(&mut buf);
        buf
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn clear_buffer_neon(buf: &mut BytesMut) {
        unsafe {
            use std::arch::aarch64::*;

            let ptr = buf.as_mut_ptr();
            let len = buf.len();

            // Use NEON for buffers >= 32 bytes
            if len >= 32 {
                let zero = vdupq_n_u8(0);
                let mut offset = 0;

                // Process 32 bytes at a time using NEON
                while offset + 32 <= len {
                    vst1q_u8(ptr.add(offset), zero);
                    vst1q_u8(ptr.add(offset + 16), zero);
                    offset += 32;
                }

                // Clear remaining bytes
                for i in offset..len {
                    *ptr.add(i) = 0;
                }
            } else {
                // Small buffer, use normal clear
                for i in 0..len {
                    *ptr.add(i) = 0;
                }
            }
        }
    }

    pub fn acquire_small(&self) -> BytesMut {
        let mut pool = self.small.lock();
        if let Some(mut buf) = pool.pop_front() {
            self.small_hits.fetch_add(1, Ordering::Relaxed);

            #[cfg(target_arch = "aarch64")]
            Self::clear_buffer_neon(&mut buf);

            #[cfg(not(target_arch = "aarch64"))]
            buf.clear();

            buf.resize(64, 0);
            buf
        } else {
            self.small_misses.fetch_add(1, Ordering::Relaxed);
            self.small_allocated.fetch_add(1, Ordering::Relaxed);

            #[cfg(target_arch = "aarch64")]
            return Self::create_zeroed_buffer(64);

            #[cfg(not(target_arch = "aarch64"))]
            {
                let mut buf = BytesMut::with_capacity(64);
                buf.resize(64, 0);
                buf
            }
        }
    }

    pub fn acquire_medium(&self) -> BytesMut {
        let mut pool = self.medium.lock();
        if let Some(mut buf) = pool.pop_front() {
            self.medium_hits.fetch_add(1, Ordering::Relaxed);

            #[cfg(target_arch = "aarch64")]
            Self::clear_buffer_neon(&mut buf);

            #[cfg(not(target_arch = "aarch64"))]
            buf.clear();

            buf.resize(1024, 0);
            buf
        } else {
            self.medium_misses.fetch_add(1, Ordering::Relaxed);
            self.medium_allocated.fetch_add(1, Ordering::Relaxed);

            #[cfg(target_arch = "aarch64")]
            return Self::create_zeroed_buffer(1024);

            #[cfg(not(target_arch = "aarch64"))]
            {
                let mut buf = BytesMut::with_capacity(1024);
                buf.resize(1024, 0);
                buf
            }
        }
    }

    pub fn acquire_large(&self) -> BytesMut {
        let mut pool = self.large.lock();
        if let Some(mut buf) = pool.pop_front() {
            self.large_hits.fetch_add(1, Ordering::Relaxed);

            #[cfg(target_arch = "aarch64")]
            Self::clear_buffer_neon(&mut buf);

            #[cfg(not(target_arch = "aarch64"))]
            buf.clear();

            buf.resize(8192, 0);
            buf
        } else {
            self.large_misses.fetch_add(1, Ordering::Relaxed);
            self.large_allocated.fetch_add(1, Ordering::Relaxed);

            #[cfg(target_arch = "aarch64")]
            return Self::create_zeroed_buffer(8192);

            #[cfg(not(target_arch = "aarch64"))]
            {
                let mut buf = BytesMut::with_capacity(8192);
                buf.resize(8192, 0);
                buf
            }
        }
    }

    pub fn release_small(&self, mut buf: BytesMut) {
        if buf.capacity() >= 32 && buf.capacity() <= 128 {
            // ARM-optimized: clear using NEON before returning to pool
            #[cfg(target_arch = "aarch64")]
            {
                buf.clear();
                buf.resize(64, 0);
                Self::clear_buffer_neon(&mut buf);
            }

            #[cfg(not(target_arch = "aarch64"))]
            {
                buf.clear();
                buf.resize(64, 0);
            }

            let mut pool = self.small.lock();

            #[cfg(target_arch = "aarch64")]
            let max_pool_size = 2000;

            #[cfg(not(target_arch = "aarch64"))]
            let max_pool_size = 1000;

            if pool.len() < max_pool_size {
                pool.push_back(buf);
            } else {
                self.small_allocated.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    pub fn release_medium(&self, mut buf: BytesMut) {
        if buf.capacity() >= 512 && buf.capacity() <= 2048 {
            #[cfg(target_arch = "aarch64")]
            {
                buf.clear();
                buf.resize(1024, 0);
                Self::clear_buffer_neon(&mut buf);
            }

            #[cfg(not(target_arch = "aarch64"))]
            {
                buf.clear();
                buf.resize(1024, 0);
            }

            let mut pool = self.medium.lock();

            #[cfg(target_arch = "aarch64")]
            let max_pool_size = 1000;

            #[cfg(not(target_arch = "aarch64"))]
            let max_pool_size = 500;

            if pool.len() < max_pool_size {
                pool.push_back(buf);
            } else {
                self.medium_allocated.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    pub fn release_large(&self, mut buf: BytesMut) {
        if buf.capacity() >= 4096 && buf.capacity() <= 16384 {
            #[cfg(target_arch = "aarch64")]
            {
                buf.clear();
                buf.resize(8192, 0);
                Self::clear_buffer_neon(&mut buf);
            }

            #[cfg(not(target_arch = "aarch64"))]
            {
                buf.clear();
                buf.resize(8192, 0);
            }

            let mut pool = self.large.lock();

            #[cfg(target_arch = "aarch64")]
            let max_pool_size = 200;

            #[cfg(not(target_arch = "aarch64"))]
            let max_pool_size = 100;

            if pool.len() < max_pool_size {
                pool.push_back(buf);
            } else {
                self.large_allocated.fetch_sub(1, Ordering::Relaxed);
            }
        }
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
        self.small.lock().clear();
        self.medium.lock().clear();
        self.large.lock().clear();
    }

    pub fn shrink_pools(&self) {
        // ARM can handle larger pools due to better memory subsystem
        #[cfg(target_arch = "aarch64")]
        {
            let mut small = self.small.lock();
            if small.len() > 1000 {
                small.truncate(1000);
            }

            let mut medium = self.medium.lock();
            if medium.len() > 500 {
                medium.truncate(500);
            }

            let mut large = self.large.lock();
            if large.len() > 100 {
                large.truncate(100);
            }
        }

        #[cfg(not(target_arch = "aarch64"))]
        {
            let mut small = self.small.lock();
            if small.len() > 500 {
                small.truncate(500);
            }

            let mut medium = self.medium.lock();
            if medium.len() > 250 {
                medium.truncate(250);
            }

            let mut large = self.large.lock();
            if large.len() > 50 {
                large.truncate(50);
            }
        }
    }
}

pub fn init_global_pool() {
    GLOBAL_POOL.get_or_init(|| BufferPool::new());

    #[cfg(target_arch = "aarch64")]
    tracing::info!("Buffer pool initialized with ARM NEON optimizations");

    #[cfg(not(target_arch = "aarch64"))]
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

// Periodic maintenance task with ARM-specific tuning
pub async fn maintenance_task() {
    use tokio::time::{interval, Duration};

    // ARM can handle more frequent maintenance due to efficient atomics
    #[cfg(target_arch = "aarch64")]
    let maintenance_interval = Duration::from_secs(30);

    #[cfg(not(target_arch = "aarch64"))]
    let maintenance_interval = Duration::from_secs(60);

    let mut interval = interval(maintenance_interval);

    loop {
        interval.tick().await;

        let pool = get_pool();
        let stats = pool.get_stats();

        // ARM-specific: more aggressive shrinking threshold
        #[cfg(target_arch = "aarch64")]
        let shrink_threshold = 98.0;

        #[cfg(not(target_arch = "aarch64"))]
        let shrink_threshold = 95.0;

        if stats.medium_hit_rate > shrink_threshold {
            pool.shrink_pools();
            tracing::debug!("Shrinking buffer pools due to high hit rate");
        }

        tracing::debug!(
            "Buffer pool stats - Small: {:.1}% hit rate ({}/{}), Medium: {:.1}% hit rate ({}/{}), Large: {:.1}% hit rate ({}/{})",
            stats.small_hit_rate,
            stats.small_pooled,
            stats.small_allocated,
            stats.medium_hit_rate,
            stats.medium_pooled,
            stats.medium_allocated,
            stats.large_hit_rate,
            stats.large_pooled,
            stats.large_allocated
        );
    }
}