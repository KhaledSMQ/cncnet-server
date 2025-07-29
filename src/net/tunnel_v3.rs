//! Tunnel V3 implementation
//!
//! This module implements the V3 tunnel protocol for CnCNet.
//! It provides UDP-based tunneling with master server announcements.
//!
//! Key features:
//! - UDP-based client-to-client tunneling
//! - Automatic client ID management
//! - Master server heartbeat announcements
//! - Command-based maintenance control
//! - Per-IP connection limiting
//! - Rate limiting for pings
//! - Graceful shutdown support
//! - Enhanced logging and error handling
//! - Low memory usage with bounded collections
//! - Backpressure control to prevent overload
//! - Connection state consistency guarantees

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;
use dashmap::DashMap;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest;
use sha1::{Digest, Sha1};
use smallvec::SmallVec;
use tokio::net::UdpSocket;
use tokio::sync::{oneshot::{self, Receiver}, Semaphore};
use tokio::time::{self, timeout};
use tracing::{debug, error, info, warn};

use crate::net::constants::*;
use crate::net::rate_limiter::{IpRateLimiterManager, RateLimiterConfig};
use crate::net::tunnel_client::TunnelClient;
use crate::net::utils::create_optimized_socket;

/// Maximum concurrent packet processing tasks
const MAX_CONCURRENT_PROCESSING: usize = 2000;

/// Timeout for UDP operations
const UDP_OPERATION_TIMEOUT: Duration = Duration::from_millis(100);

/// Maximum packet size we'll accept
const MAX_PACKET_SIZE: usize = 1024;

/// Minimum valid packet size
const MIN_PACKET_SIZE: usize = 8;

/// Default maximum pings per IP during one interval
const DEFAULT_MAX_PINGS_PER_IP: usize = 20;

/// Default maximum total pings globally during one interval
const DEFAULT_MAX_PINGS_GLOBAL: usize = 5000;

/// Default counter reset interval in milliseconds
const DEFAULT_RESET_INTERVAL_MS: u64 = 60_000;

/// HTTP client for master server announcements
static HTTP_CLIENT: once_cell::sync::Lazy<reqwest::Client> = once_cell::sync::Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(1)
        .build()
        .expect("Failed to create HTTP client")
});

/// Configuration for the Tunnel V3 service
#[derive(Debug, Clone)]
pub struct TunnelV3Config {
    /// Port used for the tunnel server
    pub port: u16,
    /// Maximum clients allowed on the tunnel server
    pub max_clients: usize,
    /// Maximum clients allowed per IP address
    pub ip_limit: usize,
    /// Name of the server
    pub name: String,
    /// Don't register to master
    pub no_master_announce: bool,
    /// Master password
    pub master_password: String,
    /// Maintenance password
    pub maintenance_password: String,
    /// Master server URL
    pub master_server_url: String,
    /// Maximum pings per IP during one reset interval
    pub max_pings_per_ip: usize,
    /// Maximum total pings globally during one reset interval
    pub max_pings_global: usize,
    /// Interval for resetting rate limiters (milliseconds)
    pub reset_interval_ms: u64,
    /// Maximum concurrent packet processing
    pub max_concurrent_processing: usize,
}

impl Default for TunnelV3Config {
    fn default() -> Self {
        Self {
            port: 50001,
            max_clients: 200,
            ip_limit: 8,
            name: "Unnamed server".to_string(),
            no_master_announce: false,
            master_password: String::new(),
            maintenance_password: "KUYn3b2z".to_string(),
            master_server_url: "http://cncnet.org/master-announce".to_string(),
            max_pings_per_ip: DEFAULT_MAX_PINGS_PER_IP,
            max_pings_global: DEFAULT_MAX_PINGS_GLOBAL,
            reset_interval_ms: DEFAULT_RESET_INTERVAL_MS,
            max_concurrent_processing: MAX_CONCURRENT_PROCESSING,
        }
    }
}

/// Service metrics for monitoring
#[derive(Debug)]
struct ServiceMetrics {
    /// Total packets received
    packets_received: AtomicU64,
    /// Total packets processed
    packets_processed: AtomicU64,
    /// Total packets dropped (rate limit)
    packets_dropped: AtomicU64,
    /// Total tunneled packets
    packets_tunneled: AtomicU64,
    /// Total new connections
    connections_created: AtomicU64,
    /// Total connection rejections
    connections_rejected: AtomicU64,
    /// Current active connections
    active_connections: AtomicUsize,
}

impl ServiceMetrics {
    fn new() -> Self {
        Self {
            packets_received: AtomicU64::new(0),
            packets_processed: AtomicU64::new(0),
            packets_dropped: AtomicU64::new(0),
            packets_tunneled: AtomicU64::new(0),
            connections_created: AtomicU64::new(0),
            connections_rejected: AtomicU64::new(0),
            active_connections: AtomicUsize::new(0),
        }
    }
}

/// Connection state for tracking IP limits
#[derive(Debug)]
struct ConnectionState {
    /// Per-IP connection counts
    ip_connections: DashMap<IpAddr, usize>,
    /// Reverse mapping: client_id -> IP
    client_to_ip: DashMap<u32, IpAddr>,
}

impl ConnectionState {
    fn new() -> Self {
        Self {
            ip_connections: DashMap::with_capacity(1024),
            client_to_ip: DashMap::with_capacity(2048),
        }
    }

    /// Tries to add a new connection for an IP
    /// Returns true if allowed, false if limit reached
    fn try_add_connection(&self, client_id: u32, ip: IpAddr, limit: usize) -> bool {
        // Check current count
        let current = self.ip_connections.get(&ip).map(|c| *c).unwrap_or(0);
        if current >= limit {
            return false;
        }

        // Add connection
        *self.ip_connections.entry(ip).or_insert(0) += 1;
        self.client_to_ip.insert(client_id, ip);
        true
    }

    /// Updates a connection when IP changes
    /// Returns true if allowed, false if new IP limit reached
    fn update_connection(&self, client_id: u32, new_ip: IpAddr, limit: usize) -> bool {
        // Get old IP
        if let Some(old_ip) = self.client_to_ip.get(&client_id).map(|ip| *ip) {
            if old_ip == new_ip {
                return true; // No change
            }

            // Check new IP limit
            let new_count = self.ip_connections.get(&new_ip).map(|c| *c).unwrap_or(0);
            if new_count >= limit {
                return false;
            }

            // Update counts atomically
            // Decrement old IP
            if let Some(mut old_count) = self.ip_connections.get_mut(&old_ip) {
                *old_count = old_count.saturating_sub(1);
                if *old_count == 0 {
                    drop(old_count);
                    self.ip_connections.remove(&old_ip);
                }
            }

            // Increment new IP
            *self.ip_connections.entry(new_ip).or_insert(0) += 1;

            // Update mapping
            self.client_to_ip.insert(client_id, new_ip);
            true
        } else {
            // No existing connection, treat as new
            self.try_add_connection(client_id, new_ip, limit)
        }
    }

    /// Removes a connection
    fn remove_connection(&self, client_id: u32) {
        if let Some((_, ip)) = self.client_to_ip.remove(&client_id) {
            if let Some(mut count) = self.ip_connections.get_mut(&ip) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    drop(count);
                    self.ip_connections.remove(&ip);
                }
            }
        }
    }

    /// Gets the current connection count for an IP
    fn get_ip_count(&self, ip: &IpAddr) -> usize {
        self.ip_connections.get(ip).map(|c| *c).unwrap_or(0)
    }
}

/// Tunnel V3 server implementation
pub struct TunnelV3 {
    /// Service configuration
    config: TunnelV3Config,
    /// Hash of maintenance password (if set)
    maintenance_password_sha1: Option<[u8; 20]>,
    /// Manager for per-IP ping rate limiting
    ping_manager: Arc<IpRateLimiterManager>,
    /// Global ping rate limiter
    global_ping_limiter: Arc<crate::net::rate_limiter::RateLimiter>,
    /// Maintenance mode flag
    maintenance_mode: Arc<AtomicBool>,
    /// Last command timestamp for rate limiting
    last_command_time: Arc<AtomicU64>,
    /// Service metrics
    metrics: Arc<ServiceMetrics>,
    /// Semaphore for backpressure control
    processing_semaphore: Arc<Semaphore>,
}

impl TunnelV3 {
    /// Creates a new Tunnel V3 instance with custom configuration
    pub fn with_config(mut config: TunnelV3Config) -> Self {
        // Normalize configuration
        if config.port <= 1024 {
            config.port = 50001;
        }
        if config.max_clients < 2 {
            config.max_clients = 200;
        }
        if config.ip_limit < 1 {
            config.ip_limit = 8;
        }
        if config.name.is_empty() {
            config.name = "Unnamed server".to_string();
        } else {
            config.name = config.name.replace(';', "");
        }

        // Hash maintenance password if provided
        let maintenance_password_sha1 = if !config.maintenance_password.is_empty() {
            let mut hasher = Sha1::new();
            hasher.update(config.maintenance_password.as_bytes());
            Some(hasher.finalize().into())
        } else {
            None
        };

        // Configure global ping rate limiter
        let global_rl_config = RateLimiterConfig {
            max_tokens: config.max_pings_global as u32,
            refill_rate: config.max_pings_global as u32,
            refill_interval_ms: config.reset_interval_ms,
            ordering: crate::net::rate_limiter::MemoryOrdering::Relaxed,
        };
        let global_ping_limiter = Arc::new(crate::net::rate_limiter::RateLimiter::with_config(global_rl_config));

        // Configure per-IP ping rate limiter manager
        let per_ip_rl_config = RateLimiterConfig {
            max_tokens: config.max_pings_per_ip as u32,
            refill_rate: config.max_pings_per_ip as u32,
            refill_interval_ms: config.reset_interval_ms,
            ordering: crate::net::rate_limiter::MemoryOrdering::Relaxed,
        };
        let ping_manager = Arc::new(IpRateLimiterManager::new(per_ip_rl_config));

        // Create processing semaphore
        let processing_semaphore = Arc::new(Semaphore::new(config.max_concurrent_processing));

        Self {
            config,
            maintenance_password_sha1,
            ping_manager,
            global_ping_limiter,
            maintenance_mode: Arc::new(AtomicBool::new(false)),
            last_command_time: Arc::new(AtomicU64::new(0)),
            metrics: Arc::new(ServiceMetrics::new()),
            processing_semaphore,
        }
    }

    /// Creates a new Tunnel V3 instance with default configuration
    pub fn new(
        port: u16,
        max_clients: usize,
        name: String,
        no_master_announce: bool,
        master_password: String,
        maintenance_password: String,
        master_server_url: String,
        ip_limit: usize,
    ) -> Self {
        let config = TunnelV3Config {
            port,
            max_clients,
            name,
            no_master_announce,
            master_password,
            maintenance_password,
            master_server_url,
            ip_limit,
            ..Default::default()
        };
        Self::with_config(config)
    }

    /// Starts the Tunnel V3 service
    ///
    /// Returns a sender for shutdown signaling and the task join handle
    pub async fn start(
        self,
    ) -> Result<(oneshot::Sender<()>, tokio::task::JoinHandle<Result<()>>)> {
        info!("Starting Tunnel V3 with config: {:?}", self.config);

        // Create optimized UDP socket
        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), self.config.port);
        let socket = Arc::new(create_optimized_socket(socket_addr).await?);

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        // Spawn the main service task
        let handle = tokio::spawn(self.run(socket, shutdown_rx));

        Ok((shutdown_tx, handle))
    }

    /// Internal run loop for the service
    async fn run(self, socket: Arc<UdpSocket>, mut shutdown_rx: Receiver<()>) -> Result<()> {
        // Client mappings (ID to client)
        let mappings = Arc::new(DashMap::<u32, TunnelClient>::with_capacity(self.config.max_clients));

        // Connection state tracking
        let connection_state = Arc::new(ConnectionState::new());

        // Start per-IP ping cleanup task
        let ping_cleanup_handle = self.ping_manager.clone().start_cleanup_task();

        // Start heartbeat task
        let heartbeat_handle = {
            let mappings = mappings.clone();
            let connection_state = connection_state.clone();
            let maintenance_mode = self.maintenance_mode.clone();
            let no_master = self.config.no_master_announce;
            let url = self.config.master_server_url.clone();
            let name = self.config.name.clone();
            let port = self.config.port;
            let max_clients = self.config.max_clients;
            let master_pw = self.config.master_password.clone();
            let metrics = self.metrics.clone();

            tokio::spawn(async move {
                let mut interval = time::interval(Duration::from_millis(MASTER_ANNOUNCE_INTERVAL));
                interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

                loop {
                    interval.tick().await;
                    Self::send_heartbeat(
                        &mappings,
                        &connection_state,
                        &maintenance_mode,
                        no_master,
                        &url,
                        &name,
                        port,
                        max_clients,
                        &master_pw,
                        &metrics,
                    )
                        .await;
                }
            })
        };

        // Send initial heartbeat
        Self::send_heartbeat(
            &mappings,
            &connection_state,
            &self.maintenance_mode,
            self.config.no_master_announce,
            &self.config.master_server_url,
            &self.config.name,
            self.config.port,
            self.config.max_clients,
            &self.config.master_password,
            &self.metrics,
        )
            .await;

        // Start metrics reporting task
        let metrics_handle = {
            let metrics = self.metrics.clone();
            tokio::spawn(async move {
                let mut interval = time::interval(Duration::from_secs(60));
                interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

                loop {
                    interval.tick().await;
                    info!(
                        "V3 metrics - received: {}, processed: {}, dropped: {}, tunneled: {}, active: {}",
                        metrics.packets_received.load(Ordering::Relaxed),
                        metrics.packets_processed.load(Ordering::Relaxed),
                        metrics.packets_dropped.load(Ordering::Relaxed),
                        metrics.packets_tunneled.load(Ordering::Relaxed),
                        metrics.active_connections.load(Ordering::Relaxed),
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
                    info!("Tunnel V3 shutting down");
                    ping_cleanup_handle.abort();
                    heartbeat_handle.abort();
                    metrics_handle.abort();
                    break Ok(());
                }

                // Receive UDP packets with timeout
                result = timeout(UDP_OPERATION_TIMEOUT, socket.recv_from(&mut recv_buf)) => {
                    match result {
                        Ok(Ok((size, addr))) => {
                            self.metrics.packets_received.fetch_add(1, Ordering::Relaxed);

                            // Basic validation
                            if size < MIN_PACKET_SIZE || size > MAX_PACKET_SIZE {
                                self.metrics.packets_dropped.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }

                            // Try to acquire processing permit (backpressure)
                            let permit = match self.processing_semaphore.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    self.metrics.packets_dropped.fetch_add(1, Ordering::Relaxed);
                                    warn!("V3 processing queue full, dropping packet");
                                    continue;
                                }
                            };

                            // Clone data for async processing
                            let data: SmallVec<[u8; 64]> = recv_buf[..size].into();
                            let socket = socket.clone();
                            let service = self.clone();
                            let mappings = mappings.clone();
                            let connection_state = connection_state.clone();

                            // Spawn task to handle packet
                            tokio::spawn(async move {
                                let _permit = permit; // Hold permit
                                service.on_receive(&data, addr, &socket, &mappings, &connection_state).await;
                            });
                        }
                        Ok(Err(e)) => {
                            error!("V3 UDP receive error: {}", e);
                        }
                        Err(_) => {
                            // Timeout - normal, continue
                        }
                    }
                }
            }
        }
    }

    /// Handles received UDP packets
    async fn on_receive(
        &self,
        data: &[u8],
        remote_addr: SocketAddr,
        socket: &UdpSocket,
        mappings: &DashMap<u32, TunnelClient>,
        connection_state: &ConnectionState,
    ) {
        // Parse header
        let sender_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let receiver_id = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

        // Handle special command packets
        if sender_id == 0 {
            if receiver_id == u32::MAX && data.len() >= 8 + 1 + 20 {
                self.execute_command(data[8], &data[9..29]).await;
            }
            if receiver_id != 0 {
                return;
            }
        }

        // Validate packet
        if !self.is_valid_packet(sender_id, receiver_id, &remote_addr) {
            self.metrics.packets_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }

        // Handle ping packets
        if sender_id == 0 && receiver_id == 0 {
            if data.len() == 50 && !self.ping_limit_reached(remote_addr.ip()) {
                if let Err(e) = socket.send_to(&data[..12], remote_addr).await {
                    debug!("Failed to send ping response: {}", e);
                }
            }
            self.metrics.packets_processed.fetch_add(1, Ordering::Relaxed);
            return;
        }

        // Handle tunnel traffic
        self.handle_tunnel_traffic(
            sender_id,
            receiver_id,
            data,
            remote_addr,
            socket,
            mappings,
            connection_state,
        ).await;

        self.metrics.packets_processed.fetch_add(1, Ordering::Relaxed);
    }

    /// Validates packet parameters
    #[inline]
    fn is_valid_packet(&self, sender_id: u32, receiver_id: u32, addr: &SocketAddr) -> bool {
        // Check for self-send (except for ID 0)
        if sender_id == receiver_id && sender_id != 0 {
            return false;
        }

        // Check for invalid source addresses
        if addr.ip().is_loopback() || addr.ip().is_unspecified() || addr.port() == 0 {
            return false;
        }

        true
    }

    /// Handles tunnel traffic between clients
    async fn handle_tunnel_traffic(
        &self,
        sender_id: u32,
        receiver_id: u32,
        data: &[u8],
        remote_addr: SocketAddr,
        socket: &UdpSocket,
        mappings: &DashMap<u32, TunnelClient>,
        connection_state: &ConnectionState,
    ) {
        // Handle existing sender
        if let Some(mut sender) = mappings.get_mut(&sender_id) {
            let existing_ep = sender.remote_ep;

            if let Some(ep) = existing_ep {
                if ep != remote_addr {
                    // IP change attempt
                    if sender.is_timed_out()
                        && !self.maintenance_mode.load(Ordering::Relaxed)
                        && connection_state.update_connection(sender_id, remote_addr.ip(), self.config.ip_limit)
                    {
                        sender.remote_ep = Some(remote_addr);
                        debug!("Client {} changed IP from {} to {}", sender_id, ep, remote_addr);
                    } else {
                        // Reject IP change
                        self.metrics.connections_rejected.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                }
            } else {
                // Set initial endpoint
                sender.remote_ep = Some(remote_addr);
            }

            sender.update_last_receive();
        } else {
            // New client connection
            if mappings.len() >= self.config.max_clients
                || self.maintenance_mode.load(Ordering::Relaxed)
                || !connection_state.try_add_connection(sender_id, remote_addr.ip(), self.config.ip_limit)
            {
                self.metrics.connections_rejected.fetch_add(1, Ordering::Relaxed);
                return;
            }

            // Create new client
            let mut client = TunnelClient::new();
            client.set_endpoint(remote_addr);
            client.update_last_receive();
            mappings.insert(sender_id, client);

            self.metrics.connections_created.fetch_add(1, Ordering::Relaxed);
            self.metrics.active_connections.fetch_add(1, Ordering::Relaxed);
            debug!("New client {} connected from {}", sender_id, remote_addr);
        }

        // Forward packet to receiver
        if let Some(receiver) = mappings.get(&receiver_id) {
            if let Some(receiver_ep) = receiver.remote_ep {
                if receiver_ep != remote_addr {
                    if let Err(e) = socket.send_to(data, receiver_ep).await {
                        debug!("Failed to forward packet: {}", e);
                    } else {
                        self.metrics.packets_tunneled.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    /// Checks if ping limit has been reached for an IP
    #[inline]
    fn ping_limit_reached(&self, ip: IpAddr) -> bool {
        !self.global_ping_limiter.try_acquire() || !self.ping_manager.try_acquire(ip)
    }

    /// Executes a command packet
    async fn execute_command(&self, command: u8, data: &[u8]) {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Check rate limit
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let last = self.last_command_time.load(Ordering::Relaxed);
        if now - last < COMMAND_RATE_LIMIT {
            return;
        }

        // Verify password
        if let Some(expected_hash) = self.maintenance_password_sha1 {
            let provided_hash: [u8; 20] = match data.try_into() {
                Ok(hash) => hash,
                Err(_) => return,
            };

            if provided_hash != expected_hash {
                return;
            }
        } else {
            return;
        }

        self.last_command_time.store(now, Ordering::Relaxed);

        // Execute command
        match command {
            0 => {
                // MaintenanceMode toggle
                let current = self.maintenance_mode.load(Ordering::Relaxed);
                self.maintenance_mode.store(!current, Ordering::Relaxed);
                info!("Maintenance mode toggled to: {}", !current);
            }
            _ => warn!("Unknown command: {}", command),
        }
    }

    /// Sends heartbeat to master server and cleans up expired connections
    async fn send_heartbeat(
        mappings: &DashMap<u32, TunnelClient>,
        connection_state: &ConnectionState,
        maintenance_mode: &AtomicBool,
        no_master_announce: bool,
        master_url: &str,
        name: &str,
        port: u16,
        max_clients: usize,
        master_password: &str,
        metrics: &ServiceMetrics,
    ) {
        // Clean up expired mappings
        let mut removed_count = 0;
        mappings.retain(|id, client| {
            if client.is_timed_out() {
                connection_state.remove_connection(*id);
                removed_count += 1;
                false
            } else {
                true
            }
        });

        if removed_count > 0 {
            metrics.active_connections.fetch_sub(removed_count, Ordering::Relaxed);
            debug!("Cleaned up {} timed out clients", removed_count);
        }

        let client_count = mappings.len();
        debug!("V3 Heartbeat: {} clients connected", client_count);

        // Send to master server
        if !no_master_announce {
            let maintenance = if maintenance_mode.load(Ordering::Relaxed) {
                "1"
            } else {
                "0"
            };

            let url = format!(
                "{}?version=3&name={}&port={}&clients={}&maxclients={}&masterpw={}&maintenance={}",
                master_url,
                utf8_percent_encode(name, NON_ALPHANUMERIC),
                port,
                client_count,
                max_clients,
                utf8_percent_encode(master_password, NON_ALPHANUMERIC),
                maintenance
            );

            match HTTP_CLIENT.get(&url).send().await {
                Ok(_) => debug!("V3 Heartbeat sent successfully"),
                Err(e) => error!("V3 Heartbeat error: {}", e),
            }
        }
    }

    /// Gets current service metrics
    pub fn metrics(&self) -> V3Metrics {
        V3Metrics {
            packets_received: self.metrics.packets_received.load(Ordering::Relaxed),
            packets_processed: self.metrics.packets_processed.load(Ordering::Relaxed),
            packets_dropped: self.metrics.packets_dropped.load(Ordering::Relaxed),
            packets_tunneled: self.metrics.packets_tunneled.load(Ordering::Relaxed),
            connections_created: self.metrics.connections_created.load(Ordering::Relaxed),
            connections_rejected: self.metrics.connections_rejected.load(Ordering::Relaxed),
            active_connections: self.metrics.active_connections.load(Ordering::Relaxed),
            active_ips: self.ping_manager.active_ips(),
        }
    }
}

/// Clone implementation for TunnelV3
impl Clone for TunnelV3 {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            maintenance_password_sha1: self.maintenance_password_sha1,
            ping_manager: self.ping_manager.clone(),
            global_ping_limiter: self.global_ping_limiter.clone(),
            maintenance_mode: self.maintenance_mode.clone(),
            last_command_time: self.last_command_time.clone(),
            metrics: self.metrics.clone(),
            processing_semaphore: self.processing_semaphore.clone(),
        }
    }
}

/// Public metrics structure
#[derive(Debug, Clone)]
pub struct V3Metrics {
    pub packets_received: u64,
    pub packets_processed: u64,
    pub packets_dropped: u64,
    pub packets_tunneled: u64,
    pub connections_created: u64,
    pub connections_rejected: u64,
    pub active_connections: usize,
    pub active_ips: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_start_shutdown() {
        let config = TunnelV3Config::default();
        let service = TunnelV3::with_config(config);
        let (shutdown_tx, handle) = service.start().await.unwrap();

        sleep(Duration::from_millis(100)).await;
        shutdown_tx.send(()).unwrap();
        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn test_ping_limit_reached() {
        let config = TunnelV3Config::default();
        let service = TunnelV3::with_config(config);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        // Should allow up to max_pings_per_ip
        for _ in 0..DEFAULT_MAX_PINGS_PER_IP {
            assert!(!service.ping_limit_reached(ip));
        }
        assert!(service.ping_limit_reached(ip));
    }

    #[test]
    fn test_connection_state() {
        let state = ConnectionState::new();
        let ip1 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let ip2 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));

        // Test adding connections
        assert!(state.try_add_connection(1, ip1, 2));
        assert!(state.try_add_connection(2, ip1, 2));
        assert!(!state.try_add_connection(3, ip1, 2)); // Limit reached

        // Test updating connection
        assert!(state.update_connection(1, ip2, 2));
        assert_eq!(state.get_ip_count(&ip1), 1);
        assert_eq!(state.get_ip_count(&ip2), 1);

        // Test removing connection
        state.remove_connection(1);
        assert_eq!(state.get_ip_count(&ip2), 0);
    }

    #[test]
    fn test_packet_validation() {
        let service = TunnelV3::with_config(TunnelV3Config::default());

        let valid_addr: SocketAddr = "192.168.1.1:1234".parse().unwrap();
        let loopback: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let zero_port: SocketAddr = "192.168.1.1:0".parse().unwrap();

        // Valid packets
        assert!(service.is_valid_packet(1, 2, &valid_addr));
        assert!(service.is_valid_packet(0, 0, &valid_addr));

        // Invalid packets
        assert!(!service.is_valid_packet(1, 1, &valid_addr)); // Self-send
        assert!(!service.is_valid_packet(1, 2, &loopback)); // Loopback
        assert!(!service.is_valid_packet(1, 2, &zero_port)); // Zero port
    }
}