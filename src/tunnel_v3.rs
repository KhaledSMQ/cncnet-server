use crate::config::SharedConfig;
use crate::metrics::{LocalMetricsBatch, Metrics};
use crate::rate_limiter::{ConnectionRateLimiter, RateLimiter};
use udp_relay_core::{create_dashmap_with_capacity, validate_address, QualityAnalyzer, TunnelClient};

use dashmap::DashMap;
use sha1::{Digest, Sha1};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::task::JoinSet;
use tokio::time;
use tracing::{debug, error, info, warn};

const VERSION: u8 = 3;
const MASTER_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(60);
const COMMAND_RATE_LIMIT_SECS: u64 = 60;
const MAX_PINGS_PER_IP: usize = 20;
const TIMEOUT_SECONDS: u64 = 30;
const MAX_PACKET_SIZE: usize = 1472;

pub struct TunnelV3 {
    config: SharedConfig,
    metrics: Arc<Metrics>,
    mappings: Arc<DashMap<u32, Arc<TunnelClient>>>,
    connection_limiter: Arc<ConnectionRateLimiter>,
    ping_limiter: Arc<RateLimiter>,
    maintenance_mode: Arc<AtomicBool>,
    last_command_tick: Arc<AtomicU64>,
    boot_instant: Instant,
    maintenance_password_sha1: Option<Vec<u8>>,
    http_client: reqwest::Client,
    quality_analyzers: Arc<DashMap<u32, QualityAnalyzer>>,
}

impl TunnelV3 {
    pub fn new(config: SharedConfig, metrics: Arc<Metrics>) -> Self {
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
            .local_address("0.0.0.0".parse().ok())
            .build()
            .expect("Failed to create HTTP client");

        Self {
            connection_limiter: Arc::new(ConnectionRateLimiter::new(config.ip_limit as u32)),
            ping_limiter: Arc::new(RateLimiter::new(60, MAX_PINGS_PER_IP as u32)),
            mappings: Arc::new(create_dashmap_with_capacity(config.max_clients)),
            quality_analyzers: Arc::new(create_dashmap_with_capacity(config.max_clients)),
            config,
            metrics,
            maintenance_mode: Arc::new(AtomicBool::new(false)),
            last_command_tick: Arc::new(AtomicU64::new(0)),
            boot_instant: Instant::now(),
            maintenance_password_sha1,
            http_client,
        }
    }

    fn create_bound_socket(&self) -> crate::errors::Result<UdpSocket> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

        socket.set_reuse_address(true)?;

        #[cfg(unix)]
        {
            socket.set_reuse_port(true)?;
        }

        #[cfg(target_arch = "aarch64")]
        {
            let _ = socket.set_recv_buffer_size(32 * 1024 * 1024);
            let _ = socket.set_send_buffer_size(32 * 1024 * 1024);
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

    pub async fn start(self: Arc<Self>) -> crate::errors::Result<()> {
        info!("Tunnel V3 listening on port {}", self.config.tunnel_port);

        let mut tasks = JoinSet::new();

        let heartbeat_self = self.clone();
        tasks.spawn(async move {
            heartbeat_self.heartbeat_loop().await;
        });

        let num_receivers = num_cpus::get().min(4).max(2);

        info!("Starting {} V3 receive workers (per-socket)", num_receivers);

        for worker_id in 0..num_receivers {
            let self_clone = self.clone();
            let worker_socket = Arc::new(self_clone.create_bound_socket()?);

            tasks.spawn(async move {
                self_clone.receive_loop(worker_socket, worker_id).await;
            });
        }

        while let Some(res) = tasks.join_next().await {
            if let Err(e) = res {
                error!("V3 task panicked: {}", e);
            }
        }

        Ok(())
    }

    async fn receive_loop(&self, socket: Arc<UdpSocket>, worker_id: usize) {
        let mut buf = [0u8; MAX_PACKET_SIZE];
        let mut receive_errors = 0u32;
        let mut batch = LocalMetricsBatch::new();

        info!("V3 worker {} started", worker_id);

        loop {
            match socket.recv_from(&mut buf).await {
                Ok((size, addr)) => {
                    receive_errors = 0;
                    if size >= 8 && size <= MAX_PACKET_SIZE {
                        self.on_receive(&buf[..size], addr, &socket, &mut batch).await;
                        batch.maybe_flush(&self.metrics);
                    } else {
                        self.metrics.invalid_packets.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    batch.flush(&self.metrics);
                    receive_errors += 1;
                    if receive_errors > 100 {
                        error!("V3 worker {} excessive receive errors: {}", worker_id, e);
                        receive_errors = 0;
                    }
                    tokio::task::yield_now().await;
                }
            }
        }
    }

    #[inline(always)]
    fn parse_packet_header(packet: &[u8]) -> Option<(u32, u32)> {
        if packet.len() < 8 {
            return None;
        }

        #[cfg(target_arch = "aarch64")]
        unsafe {
            let ptr = packet.as_ptr();
            let sender_id = (ptr as *const u32).read_unaligned().to_le();
            let receiver_id = (ptr.add(4) as *const u32).read_unaligned().to_le();
            return Some((sender_id, receiver_id));
        }

        #[cfg(not(target_arch = "aarch64"))]
        {
            let sender_id = u32::from_le_bytes([packet[0], packet[1], packet[2], packet[3]]);
            let receiver_id = u32::from_le_bytes([packet[4], packet[5], packet[6], packet[7]]);
            Some((sender_id, receiver_id))
        }
    }

    async fn on_receive(&self, packet: &[u8], remote_addr: SocketAddr, socket: &UdpSocket, batch: &mut LocalMetricsBatch) {
        let (sender_id, receiver_id) = match Self::parse_packet_header(packet) {
            Some(ids) => ids,
            None => return,
        };

        batch.record_v3_rx(packet.len() as u64);

        if sender_id == 0 && receiver_id == u32::MAX && packet.len() >= 29 {
            self.execute_command(packet).await;
            return;
        }

        if !self.validate_packet(sender_id, receiver_id, &remote_addr) {
            return;
        }

        if sender_id == 0 && receiver_id == 0 {
            if packet.len() == 50 && self.ping_limiter.check_and_increment(&remote_addr.ip()) {
                self.handle_ping(packet, remote_addr, socket, batch).await;
            }
            return;
        }

        if self.maintenance_mode.load(Ordering::Acquire) {
            return;
        }

        let now = TunnelClient::current_timestamp();
        self.handle_data_forward(sender_id, receiver_id, packet, remote_addr, socket, now, batch)
            .await;
    }

    #[inline(always)]
    fn validate_packet(&self, sender_id: u32, receiver_id: u32, addr: &SocketAddr) -> bool {
        if sender_id == receiver_id && sender_id != 0 {
            return false;
        }
        validate_address(addr)
    }

    async fn handle_ping(&self, packet: &[u8], remote_addr: SocketAddr, socket: &UdpSocket, batch: &mut LocalMetricsBatch) {
        self.metrics
            .v3_ping_requests
            .fetch_add(1, Ordering::Relaxed);

        let response = &packet[..12.min(packet.len())];

        if let Err(e) = socket.send_to(response, remote_addr).await {
            debug!("Failed to send V3 ping response to {}: {}", remote_addr, e);
        } else {
            batch.record_v3_tx(response.len() as u64);
        }
    }

    async fn handle_data_forward(
        &self,
        sender_id: u32,
        receiver_id: u32,
        packet: &[u8],
        remote_addr: SocketAddr,
        socket: &UdpSocket,
        now: u64,
        batch: &mut LocalMetricsBatch,
    ) {
        let sender_valid = if let Some(existing) = self.mappings.get(&sender_id) {
            let client = existing.value().clone();
            drop(existing);

            if let Some(ep) = client.remote_ep {
                if ep != remote_addr {
                    if client.is_timed_out() {
                        if self
                            .connection_limiter
                            .try_acquire(&remote_addr.ip())
                        {
                            let new_client = Arc::new(TunnelClient::new_with_endpoint(
                                remote_addr,
                                TIMEOUT_SECONDS,
                            ));
                            self.mappings.insert(sender_id, new_client);
                            self.quality_analyzers
                                .insert(sender_id, QualityAnalyzer::new());
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    client.set_last_receive_tick_at(now);
                    client.update_stats(packet.len(), 0, now);
                    if let Some(analyzer) = self.quality_analyzers.get_mut(&sender_id) {
                        analyzer.record_packet(false);
                        drop(analyzer);
                    }
                    true
                }
            } else {
                false
            }
        } else if self.mappings.len() < self.config.max_clients {
            if self.connection_limiter.try_acquire(&remote_addr.ip()) {
                let client =
                    Arc::new(TunnelClient::new_with_endpoint(remote_addr, TIMEOUT_SECONDS));
                self.mappings.insert(sender_id, client);
                self.quality_analyzers
                    .insert(sender_id, QualityAnalyzer::new());
                self.metrics
                    .v3_active_clients
                    .store(self.mappings.len(), Ordering::Release);
                true
            } else {
                self.metrics
                    .rate_limit_hits
                    .fetch_add(1, Ordering::Relaxed);
                false
            }
        } else {
            self.metrics
                .dropped_packets
                .fetch_add(1, Ordering::Relaxed);
            false
        };

        if !sender_valid {
            return;
        }

        let forward_info = self.mappings.get(&receiver_id).and_then(|receiver| {
            let ep = receiver.remote_ep?;
            if ep == remote_addr {
                return None;
            }
            let client = receiver.value().clone();
            Some((ep, client))
        });

        if let Some((receiver_ep, receiver_client)) = forward_info {
            if let Err(e) = socket.send_to(packet, receiver_ep).await {
                debug!("Failed to forward V3 packet to {}: {}", receiver_ep, e);
                if let Some(analyzer) = self.quality_analyzers.get_mut(&receiver_id) {
                    analyzer.record_packet(true);
                }
            } else {
                batch.record_v3_tx(packet.len() as u64);
                receiver_client.update_stats(0, packet.len(), now);
            }
        }
    }

    async fn execute_command(&self, packet: &[u8]) {
        let elapsed_secs = self.boot_instant.elapsed().as_secs();
        let last = self.last_command_tick.load(Ordering::Acquire);

        if elapsed_secs.saturating_sub(last) < COMMAND_RATE_LIMIT_SECS
            || self.maintenance_password_sha1.is_none()
        {
            return;
        }

        self.last_command_tick.store(elapsed_secs, Ordering::Release);

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

            if !self.config.no_master_announce {
                self.send_master_announce().await;
            }

            if cleanup_counter % 5 == 0 {
                self.log_metrics();
            }
        }
    }

    async fn cleanup_expired_mappings(&self) {
        self.mappings.retain(|key, value| {
            if value.is_timed_out() {
                self.quality_analyzers.remove(key);
                false
            } else {
                true
            }
        });

        self.metrics
            .v3_active_clients
            .store(self.mappings.len(), Ordering::Release);
        self.ping_limiter.reset();

        info!(
            "V3 cleanup: {} active clients, {} IPs tracked",
            self.mappings.len(),
            self.connection_limiter.get_active_ip_count()
        );
    }

    async fn send_master_announce(&self) {
        let clients = self.mappings.len();
        let maintenance = if self.maintenance_mode.load(Ordering::Acquire) {
            "1"
        } else {
            "0"
        };

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
                    debug!("V3 master announce sent");
                } else {
                    warn!("V3 master announce failed: {}", resp.status());
                }
            }
            Err(e) => warn!("V3 master announce error: {}", e),
        }
    }

    fn log_metrics(&self) {
        info!(
            "V3 Stats - Clients: {}, RX: {}, TX: {}, Dropped: {}",
            self.mappings.len(),
            self.metrics.v3_packets_received.load(Ordering::Relaxed),
            self.metrics.v3_packets_sent.load(Ordering::Relaxed),
            self.metrics.dropped_packets.load(Ordering::Relaxed),
        );
    }
}
