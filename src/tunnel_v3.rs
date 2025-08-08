use crate::buffer_pool::{self, BufferGuard};
use crate::config::SharedConfig;
use crate::errors::{Result, ServerError};
use crate::metrics::Metrics;
use crate::rate_limiter::{ConnectionLimiter, RateLimiter};
use crate::tunnel_client::{TunnelClient, QualityAnalyzer};
use bytes::{Buf, BufMut};
use dashmap::DashMap;
use sha1::{Digest, Sha1};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::Semaphore;
use tokio::time;
use tracing::{debug, error, info, warn};

const VERSION: u8 = 3;
const MASTER_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(60);
const COMMAND_RATE_LIMIT: u64 = 60;
const MAX_PINGS_PER_IP: usize = 20;
const MAX_PINGS_GLOBAL: usize = 5000;
const TIMEOUT_SECONDS: u64 = 30;

// ARM-optimized batch size (better cache utilization)
#[cfg(target_arch = "aarch64")]
const BATCH_SIZE: usize = 64;

#[cfg(not(target_arch = "aarch64"))]
const BATCH_SIZE: usize = 32;

const MAX_PACKET_SIZE: usize = 1472;

// ARM-optimized memory ordering
#[cfg(target_arch = "aarch64")]
const LOAD_ORDERING: Ordering = Ordering::Acquire;

#[cfg(target_arch = "aarch64")]
const STORE_ORDERING: Ordering = Ordering::Release;

#[cfg(not(target_arch = "aarch64"))]
const LOAD_ORDERING: Ordering = Ordering::SeqCst;

#[cfg(not(target_arch = "aarch64"))]
const STORE_ORDERING: Ordering = Ordering::SeqCst;

// Helper function to create DashMap with proper shard count
fn create_dashmap_with_capacity<K, V>(capacity: usize) -> DashMap<K, V>
where
    K: Eq + std::hash::Hash,
{
    // Ensure we have at least 2 shards (DashMap requirement)
    // Use power of 2 for better performance
    let num_cpus = num_cpus::get().max(2);
    let shard_amount = if num_cpus <= 2 {
        4  // Minimum 4 shards for safety
    } else if num_cpus <= 4 {
        8
    } else if num_cpus <= 8 {
        16
    } else {
        32
    };

    DashMap::with_capacity_and_shard_amount(capacity, shard_amount)
}

// Backpressure controller with ARM optimizations
pub struct BackpressureController {
    current_load: AtomicU64,
    max_load: u64,
    rejection_probability: AtomicU32,
    packets_dropped: AtomicU64,
}

impl BackpressureController {
    pub fn new(max_load: u64) -> Self {
        Self {
            current_load: AtomicU64::new(0),
            max_load,
            rejection_probability: AtomicU32::new(0),
            packets_dropped: AtomicU64::new(0),
        }
    }

    #[inline(always)]
    pub fn should_accept(&self) -> bool {
        let load = self.current_load.load(LOAD_ORDERING);

        if load < (self.max_load * 70) / 100 {
            return true;
        }

        let load_percentage = (load * 100) / self.max_load;
        let rejection_prob = if load_percentage >= 70 {
            ((load_percentage - 70) * 1000) / 30
        } else {
            0
        };

        self.rejection_probability.store(rejection_prob as u32, STORE_ORDERING);

        use rand::Rng;
        let accept = rand::thread_rng().gen_range(0..1000) >= rejection_prob;

        if !accept {
            self.packets_dropped.fetch_add(1, Ordering::Relaxed);
        }

        accept
    }

    #[inline(always)]
    pub fn increment_load(&self) -> bool {
        let prev = self.current_load.fetch_add(1, Ordering::AcqRel);
        prev < self.max_load
    }

    #[inline(always)]
    pub fn decrement_load(&self) {
        self.current_load.fetch_sub(1, Ordering::AcqRel);
    }

    pub fn get_load_percentage(&self) -> f64 {
        let load = self.current_load.load(LOAD_ORDERING);
        (load as f64 / self.max_load as f64) * 100.0
    }
}

#[derive(Clone)]
pub struct TunnelV3 {
    config: SharedConfig,
    metrics: Arc<Metrics>,
    mappings: Arc<DashMap<u32, Arc<TunnelClient>>>,
    connection_limiter: Arc<ConnectionLimiter>,
    ping_limiter: Arc<RateLimiter>,
    maintenance_mode: Arc<AtomicBool>,
    last_command_tick: Arc<AtomicU64>,
    maintenance_password_sha1: Option<Vec<u8>>,
    task_limiter: Arc<Semaphore>,
    http_client: reqwest::Client,
    backpressure: Arc<BackpressureController>,
    quality_analyzers: Arc<DashMap<u32, QualityAnalyzer>>,
}

impl TunnelV3 {
    pub fn new(
        config: SharedConfig,
        metrics: Arc<Metrics>,
        task_limiter: Arc<Semaphore>,
    ) -> Self {
        let maintenance_password_sha1 = if !config.maintenance_password.is_empty() {
            let mut hasher = Sha1::new();
            hasher.update(config.maintenance_password.as_bytes());
            Some(hasher.finalize().to_vec())
        } else {
            None
        };

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(1)
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        // ARM can handle more concurrent connections efficiently
        #[cfg(target_arch = "aarch64")]
        let max_backpressure = 15000;

        #[cfg(not(target_arch = "aarch64"))]
        let max_backpressure = 10000;

        Self {
            connection_limiter: Arc::new(ConnectionLimiter::new(config.ip_limit)),
            ping_limiter: Arc::new(RateLimiter::new(60, MAX_PINGS_PER_IP, MAX_PINGS_GLOBAL)),
            mappings: Arc::new(create_dashmap_with_capacity(config.max_clients)),
            backpressure: Arc::new(BackpressureController::new(max_backpressure)),
            quality_analyzers: Arc::new(create_dashmap_with_capacity(config.max_clients)),
            config,
            metrics,
            maintenance_mode: Arc::new(AtomicBool::new(false)),
            last_command_tick: Arc::new(AtomicU64::new(0)),
            maintenance_password_sha1,
            task_limiter,
            http_client,
        }
    }

    pub async fn start(self: Arc<Self>) -> Result<()> {
        let socket = self.create_optimized_socket().await?;
        let socket = Arc::new(socket);

        info!("Tunnel V3 listening on port {}", self.config.tunnel_port);

        // Spawn heartbeat task
        let heartbeat_self = self.clone();
        tokio::spawn(async move {
            heartbeat_self.heartbeat_loop().await;
        });

        // Spawn buffer pool maintenance
        tokio::spawn(buffer_pool::maintenance_task());

        // ARM: Use more receive workers for better core utilization
        #[cfg(target_arch = "aarch64")]
        let num_receivers = num_cpus::get().min(4).max(2);

        #[cfg(not(target_arch = "aarch64"))]
        let num_receivers = num_cpus::get().min(8).max(2);

        let mut receivers = Vec::with_capacity(num_receivers);

        info!("Starting {} receive workers (ARM optimized)", num_receivers);

        for worker_id in 0..num_receivers {
            let self_clone = self.clone();
            let socket_clone = socket.clone();

            receivers.push(tokio::spawn(async move {
                self_clone.receive_loop(socket_clone, worker_id).await
            }));
        }

        for receiver in receivers {
            let _ = receiver.await;
        }

        Ok(())
    }

    async fn create_optimized_socket(&self) -> Result<UdpSocket> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

        socket.set_reuse_address(true)?;

        #[cfg(unix)]
        {
            socket.set_reuse_port(true)?;

            // ARM-specific: Enable SO_REUSEPORT with better load balancing
            #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
            {
                use std::os::unix::io::AsRawFd;
                use libc::{c_int, c_void, setsockopt, SOL_SOCKET};

                const SO_REUSEPORT: c_int = 15;

                let optval: c_int = 1;
                unsafe {
                    setsockopt(
                        socket.as_raw_fd(),
                        SOL_SOCKET,
                        SO_REUSEPORT,
                        &optval as *const c_int as *const c_void,
                        std::mem::size_of::<c_int>() as libc::socklen_t,
                    );
                }
            }
        }

        // ARM has excellent memory bandwidth - use larger buffers
        #[cfg(target_arch = "aarch64")]
        {
            let _ = socket.set_recv_buffer_size(32 * 1024 * 1024); // 32MB
            let _ = socket.set_send_buffer_size(32 * 1024 * 1024); // 32MB
        }

        #[cfg(not(target_arch = "aarch64"))]
        {
            let _ = socket.set_recv_buffer_size(16 * 1024 * 1024);
            let _ = socket.set_send_buffer_size(16 * 1024 * 1024);
        }

        let addr = SocketAddr::from(([0, 0, 0, 0], self.config.tunnel_port));
        socket.bind(&addr.into())?;
        socket.set_nonblocking(true)?;

        Ok(UdpSocket::from_std(socket.into())?)
    }

    async fn receive_loop(&self, socket: Arc<UdpSocket>, worker_id: usize) {
        let pool = buffer_pool::get_pool();
        let mut batch = Vec::with_capacity(BATCH_SIZE);
        let mut receive_errors = 0u32;

        info!("Worker {} started (ARM optimized)", worker_id);

        loop {
            if !self.backpressure.should_accept() {
                // ARM: Shorter sleep due to better context switching
                #[cfg(target_arch = "aarch64")]
                tokio::time::sleep(Duration::from_micros(50)).await;

                #[cfg(not(target_arch = "aarch64"))]
                tokio::time::sleep(Duration::from_micros(100)).await;

                continue;
            }

            let mut buf = pool.acquire_medium();

            match socket.recv_from(&mut buf).await {
                Ok((size, addr)) => {
                    receive_errors = 0;

                    if size >= 8 && size <= MAX_PACKET_SIZE {
                        buf.truncate(size);
                        batch.push((buf, addr));

                        if batch.len() >= BATCH_SIZE {
                            self.process_batch(&socket, &mut batch, worker_id).await;
                        }
                    } else {
                        pool.release_medium(buf);
                        self.metrics.invalid_packets.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if !batch.is_empty() {
                        self.process_batch(&socket, &mut batch, worker_id).await;
                    }
                    tokio::time::sleep(Duration::from_micros(10)).await;
                }
                Err(e) => {
                    receive_errors += 1;
                    if receive_errors > 100 {
                        error!("Worker {} excessive UDP receive errors: {}", worker_id, e);
                        receive_errors = 0;
                    }
                    pool.release_medium(buf);
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        }
    }

    // ARM-optimized packet header parsing
    #[inline(always)]
    fn parse_packet_header(packet: &[u8]) -> Option<(u32, u32)> {
        if packet.len() < 8 {
            return None;
        }

        #[cfg(target_arch = "aarch64")]
        {
            // ARM handles unaligned loads efficiently
            unsafe {
                let ptr = packet.as_ptr();
                let sender_id = (ptr as *const u32).read_unaligned().to_le();
                let receiver_id = (ptr.add(4) as *const u32).read_unaligned().to_le();
                Some((sender_id, receiver_id))
            }
        }

        #[cfg(not(target_arch = "aarch64"))]
        {
            let sender_id = u32::from_le_bytes([packet[0], packet[1], packet[2], packet[3]]);
            let receiver_id = u32::from_le_bytes([packet[4], packet[5], packet[6], packet[7]]);
            Some((sender_id, receiver_id))
        }
    }

    async fn process_batch(
        &self,
        socket: &Arc<UdpSocket>,
        batch: &mut Vec<(bytes::BytesMut, SocketAddr)>,
        worker_id: usize,
    ) {
        let pool = buffer_pool::get_pool();

        for (buf, addr) in batch.drain(..) {
            if !self.backpressure.increment_load() {
                self.metrics.dropped_packets.fetch_add(1, Ordering::Relaxed);
                pool.release_medium(buf);
                continue;
            }

            let permit = match self.task_limiter.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    self.metrics.dropped_packets.fetch_add(1, Ordering::Relaxed);
                    self.backpressure.decrement_load();
                    pool.release_medium(buf);
                    continue;
                }
            };

            let self_clone = self.clone();
            let socket_clone = socket.clone();

            tokio::spawn(async move {
                let _permit = permit;
                let _guard = BackpressureGuard::new(self_clone.backpressure.clone());
                self_clone.on_receive(buf, addr, socket_clone).await;
            });
        }

        // Log metrics less frequently on ARM (better performance)
        #[cfg(target_arch = "aarch64")]
        const LOG_INTERVAL: u64 = 5000;

        #[cfg(not(target_arch = "aarch64"))]
        const LOG_INTERVAL: u64 = 1000;

        static BATCH_COUNTER: AtomicU64 = AtomicU64::new(0);
        if BATCH_COUNTER.fetch_add(1, Ordering::Relaxed) % LOG_INTERVAL == 0 {
            debug!(
                "Worker {} - Backpressure: {:.1}%, Dropped: {}",
                worker_id,
                self.backpressure.get_load_percentage(),
                self.backpressure.packets_dropped.load(Ordering::Relaxed)
            );
        }
    }

    async fn on_receive(
        &self,
        mut packet: bytes::BytesMut,
        remote_addr: SocketAddr,
        socket: Arc<UdpSocket>,
    ) {
        let pool = buffer_pool::get_pool();
        let _buffer_guard = BufferReleaseGuard::new(packet.clone(), pool);

        // Use optimized header parsing
        let (sender_id, receiver_id) = match Self::parse_packet_header(&packet) {
            Some(ids) => ids,
            None => return,
        };

        self.metrics.v3_packets_received.fetch_add(1, Ordering::Relaxed);
        self.metrics.v3_bytes_received.fetch_add(packet.len() as u64, Ordering::Relaxed);

        // Fast path for commands
        if sender_id == 0 && receiver_id == u32::MAX && packet.len() >= 29 {
            self.execute_command(&packet).await;
            return;
        }

        if !self.validate_packet(sender_id, receiver_id, &remote_addr) {
            return;
        }

        // Fast path for ping
        if sender_id == 0 && receiver_id == 0 {
            if packet.len() == 50 && self.ping_limiter.check_and_increment(&remote_addr.ip()) {
                self.handle_ping(&packet, remote_addr, socket).await;
            }
            return;
        }

        if self.maintenance_mode.load(LOAD_ORDERING) {
            return;
        }

        self.handle_data_forward_with_priority(sender_id, receiver_id, packet, remote_addr, socket).await;
    }

    #[inline(always)]
    fn validate_packet(&self, sender_id: u32, receiver_id: u32, addr: &SocketAddr) -> bool {
        if sender_id == receiver_id && sender_id != 0 {
            return false;
        }

        let ip_valid = match addr.ip() {
            IpAddr::V4(v4) => {
                !v4.is_loopback() &&
                    !v4.is_unspecified() &&
                    !v4.is_broadcast() &&
                    !v4.is_multicast()
            }
            IpAddr::V6(v6) => {
                !v6.is_loopback() &&
                    !v6.is_unspecified() &&
                    !v6.is_multicast()
            }
        };

        ip_valid && addr.port() != 0
    }

    async fn handle_ping(
        &self,
        packet: &[u8],
        remote_addr: SocketAddr,
        socket: Arc<UdpSocket>,
    ) {
        self.metrics.v3_ping_requests.fetch_add(1, Ordering::Relaxed);

        let response = &packet[..12.min(packet.len())];

        if let Err(e) = socket.send_to(response, remote_addr).await {
            debug!("Failed to send ping response to {}: {}", remote_addr, e);
        } else {
            self.metrics.v3_packets_sent.fetch_add(1, Ordering::Relaxed);
            self.metrics.v3_bytes_sent.fetch_add(response.len() as u64, Ordering::Relaxed);
        }
    }

    async fn handle_data_forward_with_priority(
        &self,
        sender_id: u32,
        receiver_id: u32,
        packet: bytes::BytesMut,
        remote_addr: SocketAddr,
        socket: Arc<UdpSocket>,
    ) {
        let sender_valid = if let Some(existing) = self.mappings.get(&sender_id) {
            let client = existing.clone();

            if let Some(ep) = client.remote_ep {
                if ep != remote_addr {
                    if client.is_timed_out() {
                        if self.connection_limiter.update_ip(&ep.ip(), &remote_addr.ip()) {
                            drop(existing);
                            let new_client = Arc::new(TunnelClient::new_with_endpoint(
                                remote_addr,
                                TIMEOUT_SECONDS,
                            ));
                            self.mappings.insert(sender_id, new_client.clone());
                            self.quality_analyzers.insert(sender_id, QualityAnalyzer::new());
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    client.set_last_receive_tick();
                    client.update_stats(packet.len(), 0);

                    if let Some(mut analyzer) = self.quality_analyzers.get_mut(&sender_id) {
                        analyzer.record_packet(false);
                    }
                    true
                }
            } else {
                false
            }
        } else {
            if self.mappings.len() < self.config.max_clients {
                if self.connection_limiter.try_add(&remote_addr.ip()) {
                    let client = Arc::new(TunnelClient::new_with_endpoint(
                        remote_addr,
                        TIMEOUT_SECONDS,
                    ));
                    self.mappings.insert(sender_id, client);
                    self.quality_analyzers.insert(sender_id, QualityAnalyzer::new());
                    self.metrics.v3_active_clients.store(self.mappings.len(), STORE_ORDERING);
                    true
                } else {
                    self.metrics.rate_limit_hits.fetch_add(1, Ordering::Relaxed);
                    false
                }
            } else {
                self.metrics.dropped_packets.fetch_add(1, Ordering::Relaxed);
                false
            }
        };

        if !sender_valid {
            return;
        }

        if let Some(receiver) = self.mappings.get(&receiver_id) {
            if let Some(receiver_ep) = receiver.remote_ep {
                if receiver_ep != remote_addr {
                    let is_priority = receiver.is_slow_connection();

                    if is_priority || self.backpressure.get_load_percentage() < 90.0 {
                        if let Err(e) = socket.send_to(&packet, receiver_ep).await {
                            debug!("Failed to forward packet to {}: {}", receiver_ep, e);

                            if let Some(mut analyzer) = self.quality_analyzers.get_mut(&receiver_id) {
                                analyzer.record_packet(true);
                            }
                        } else {
                            self.metrics.v3_packets_sent.fetch_add(1, Ordering::Relaxed);
                            self.metrics.v3_bytes_sent.fetch_add(packet.len() as u64, Ordering::Relaxed);
                            receiver.update_stats(0, packet.len());
                        }
                    } else {
                        self.metrics.dropped_packets.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    async fn execute_command(&self, packet: &[u8]) {
        let now = Instant::now().elapsed().as_secs();
        let last = self.last_command_tick.load(LOAD_ORDERING);

        if now.saturating_sub(last) < COMMAND_RATE_LIMIT || self.maintenance_password_sha1.is_none() {
            return;
        }

        self.last_command_tick.store(now, STORE_ORDERING);

        let command = packet[8];
        let password_sha1 = &packet[9..29];

        if let Some(ref expected_sha1) = self.maintenance_password_sha1 {
            if password_sha1 == expected_sha1.as_slice() {
                match command {
                    0 => {
                        let prev = self.maintenance_mode.fetch_xor(true, Ordering::AcqRel);
                        info!("Maintenance mode toggled: {} -> {}", prev, !prev);
                    }
                    1 => {
                        info!("Forcing cleanup of expired connections");
                        self.cleanup_expired_mappings().await;
                    }
                    _ => {
                        debug!("Unknown command: {}", command);
                    }
                }
            }
        }
    }

    async fn heartbeat_loop(&self) {
        let mut interval = time::interval(MASTER_ANNOUNCE_INTERVAL);
        let mut cleanup_counter = 0u8;

        loop {
            interval.tick().await;

            cleanup_counter = cleanup_counter.wrapping_add(1);
            if cleanup_counter % 3 == 0 {
                self.cleanup_expired_mappings().await;
            }

            if cleanup_counter % 10 == 0 {
                buffer_pool::get_pool().shrink_pools();
            }

            if !self.config.no_master_announce {
                self.send_master_announce().await;
            }

            if cleanup_counter % 5 == 0 {
                self.log_metrics();
            }
        }
    }

    async fn cleanup_expired_mappings(&self) {
        let mut expired = Vec::with_capacity(32);

        // ARM: Process more entries per chunk due to better memory bandwidth
        #[cfg(target_arch = "aarch64")]
        const CHUNK_SIZE: usize = 200;

        #[cfg(not(target_arch = "aarch64"))]
        const CHUNK_SIZE: usize = 100;

        let entries: Vec<_> = self.mappings.iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect();

        for chunk in entries.chunks(CHUNK_SIZE) {
            for (key, value) in chunk {
                if value.is_timed_out() {
                    expired.push(*key);

                    if let Some(ep) = value.remote_ep {
                        self.connection_limiter.remove(&ep.ip());
                    }
                }
            }

            if expired.len() >= CHUNK_SIZE {
                for id in expired.drain(..) {
                    self.mappings.remove(&id);
                    self.quality_analyzers.remove(&id);
                }
                tokio::task::yield_now().await;
            }
        }

        for id in expired {
            self.mappings.remove(&id);
            self.quality_analyzers.remove(&id);
        }

        self.metrics.v3_active_clients.store(self.mappings.len(), STORE_ORDERING);
        self.ping_limiter.reset();

        info!(
            "Cleanup complete: {} active clients, {} connections tracked",
            self.mappings.len(),
            self.connection_limiter.get_total_connections()
        );
    }

    async fn send_master_announce(&self) {
        let clients = self.mappings.len();
        let maintenance = if self.maintenance_mode.load(LOAD_ORDERING) { "1" } else { "0" };

        let url = format!(
            "{}?version={}&name={}&port={}&clients={}&maxclients={}&masterpw={}&maintenance={}",
            self.config.master_server_url,
            VERSION,
            urlencoding::encode(&self.config.name),
            self.config.tunnel_port,
            clients,
            self.config.max_clients,
            urlencoding::encode(&self.config.master_password),
            maintenance
        );

        match self.http_client.get(&url).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    debug!("Master announce sent successfully");
                } else {
                    warn!("Master announce failed with status: {}", resp.status());
                }
            }
            Err(e) => warn!("Failed to send master announce: {}", e),
        }
    }

    fn log_metrics(&self) {
        let stats = buffer_pool::get_pool().get_stats();

        info!(
            "Tunnel V3 Stats - Clients: {}, Packets RX: {}, TX: {}, Dropped: {}, Backpressure: {:.1}%",
            self.mappings.len(),
            self.metrics.v3_packets_received.load(Ordering::Relaxed),
            self.metrics.v3_packets_sent.load(Ordering::Relaxed),
            self.metrics.dropped_packets.load(Ordering::Relaxed),
            self.backpressure.get_load_percentage()
        );

        debug!(
            "Buffer Pool - Small: {:.1}% hit, Medium: {:.1}% hit, Large: {:.1}% hit",
            stats.small_hit_rate,
            stats.medium_hit_rate,
            stats.large_hit_rate
        );
    }
}

// RAII guard for backpressure
struct BackpressureGuard {
    controller: Arc<BackpressureController>,
}

impl BackpressureGuard {
    fn new(controller: Arc<BackpressureController>) -> Self {
        Self { controller }
    }
}

impl Drop for BackpressureGuard {
    fn drop(&mut self) {
        self.controller.decrement_load();
    }
}

// RAII guard for buffer release
struct BufferReleaseGuard {
    buffer: Option<bytes::BytesMut>,
    pool: &'static buffer_pool::BufferPool,
}

impl BufferReleaseGuard {
    fn new(buffer: bytes::BytesMut, pool: &'static buffer_pool::BufferPool) -> Self {
        Self {
            buffer: Some(buffer),
            pool,
        }
    }
}

impl Drop for BufferReleaseGuard {
    fn drop(&mut self) {
        if let Some(buf) = self.buffer.take() {
            self.pool.release_medium(buf);
        }
    }
}