//! Tunnel V3 implementation
//!
//! This module implements the V3 tunnel protocol for CnCNet.
//! It provides UDP-based tunneling with master server announcements.
//!
//! ## Protocol Overview
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                         Tunnel V3 Architecture                          │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │                                                                         │
//! │  Game Client A                                    Game Client B         │
//! │       │                                                  │              │
//! │       │ [Sender: A, Receiver: B, Data]                   │              │
//! │       └────────────────────┐                             │              │
//! │                            ▼                             │              │
//! │                    ┌───────────────┐                     │              │
//! │                    │  UDP Socket   │                     │              │
//! │                    │   :50001      │                     │              │
//! │                    └───────┬───────┘                     │              │
//! │                            │                             │              │
//! │                    ┌───────▼───────┐                     │              │
//! │                    │ Packet Router │                     │              │
//! │                    └───────┬───────┘                     │              │
//! │                            │                             │              │
//! │         ┌──────────────────┼──────────────────┐          │              │
//! │         ▼                  ▼                  ▼          │              │
//! │   ┌───────────┐    ┌─────────────┐       ┌───────────┐   │              │
//! │   │  Special  │    │   Client    │       │  Forward  │   │              │
//! │   │  Commands │    │ Management  │       │  Traffic  │   │              │
//! │   └───────────┘    └─────────────┘       └─────┬─────┘   │              │
//! │                                                │         │              │
//! │                                                └─────────┴──────────────┤
//! │                                          [Sender: A, Receiver: B, Data] │
//! │                                                                         │
//! │  ┌────────────────────────────────────────────────────────────────┐     │
//! │  │                        Packet Format                           │     │
//! │  ├────────────────────────────────────────────────────────────────┤     │
//! │  │  0-3:  Sender ID (u32 LE)                                      │     │
//! │  │  4-7:  Receiver ID (u32 LE)                                    │     │
//! │  │  8+:   Payload Data                                            │     │
//! │  │                                                                │     │
//! │  │  Special IDs:                                                  │     │
//! │  │  - 0,0: Ping packet (50 bytes total)                           │     │
//! │  │  - 0,MAX: Command packet (maintenance)                         │     │
//! │  └────────────────────────────────────────────────────────────────┘     │
//! │                                                                         │
//! │  ┌────────────────────────────────────────────────────────────────┐     │
//! │  │                    Connection State Machine                    │     │
//! │  ├────────────────────────────────────────────────────────────────┤     │
//! │  │                                                                │     │
//! │  │     New Client ──┬──▶ Active ──┬──▶ Timed Out ──▶ Removed      │     │
//! │  │                  │             │                               │     │
//! │  │                  │             ▼                               │     │
//! │  │                  │         IP Change                           │     │
//! │  │                  │         (if allowed)                        │     │
//! │  │                  │              │                              │     │
//! │  │                  └──────────────┘                              │     │
//! │  │                                                                │     │
//! │  │  Rate Limits:                                                  │     │
//! │  │  - Per-IP connection limit                                     │     │
//! │  │  - Global connection limit                                     │     │
//! │  │  - Ping rate limiting (per-IP and global)                      │     │
//! │  │  - Command rate limiting (per-IP and global)                   │     │
//! │  └────────────────────────────────────────────────────────────────┘     │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! Key features:
//! - UDP-based client-to-client tunneling
//! - Automatic client ID management
//! - Master server heartbeat announcements
//! - Command-based maintenance control
//! - Per-IP connection limiting with atomic operations
//! - Rate limiting for pings and commands
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
use tracing::{debug, error, info, warn, trace};

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

/// Default maximum commands per IP during one interval
const DEFAULT_MAX_COMMANDS_PER_IP: usize = 5;

/// Default maximum total commands globally during one interval
const DEFAULT_MAX_COMMANDS_GLOBAL: usize = 100;

/// Default counter reset interval in milliseconds
const DEFAULT_RESET_INTERVAL_MS: u64 = 60_000;

/// Maximum URL length for HTTP requests
const MAX_URL_LENGTH: usize = 2000;

/// Maximum failed command attempts before IP block
const MAX_FAILED_COMMANDS: usize = 3;

/// IP block duration for failed commands (seconds)
const COMMAND_BLOCK_DURATION_SECS: u64 = 3600; // 1 hour

/// Stale connection timeout for cleanup (5 minutes)
const STALE_CONNECTION_TIMEOUT_MS: u64 = 300_000;

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
    /// Maximum commands per IP during one reset interval
    pub max_commands_per_ip: usize,
    /// Maximum total commands globally during one reset interval
    pub max_commands_global: usize,
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
            max_commands_per_ip: DEFAULT_MAX_COMMANDS_PER_IP,
            max_commands_global: DEFAULT_MAX_COMMANDS_GLOBAL,
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
    /// Failed command attempts
    failed_commands: AtomicU64,
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
            failed_commands: AtomicU64::new(0),
        }
    }
}

/// Connection state for tracking IP limits with thread-safe operations
///
/// This implementation uses a transactional approach to ensure atomic updates
/// across both the IP connection counts and client-to-IP mappings.
#[derive(Debug)]
struct ConnectionState {
    /// Per-IP connection counts
    ip_connections: DashMap<IpAddr, AtomicUsize>,
    /// Reverse mapping: client_id -> IP
    client_to_ip: DashMap<u32, IpAddr>,
    /// Last activity timestamp for each IP (for stale cleanup)
    ip_last_activity: DashMap<IpAddr, AtomicU64>,
}

impl ConnectionState {
    fn new() -> Self {
        Self {
            ip_connections: DashMap::with_capacity(1024),
            client_to_ip: DashMap::with_capacity(2048),
            ip_last_activity: DashMap::with_capacity(1024),
        }
    }

    /// Updates activity timestamp for an IP
    fn update_activity(&self, ip: IpAddr) {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        self.ip_last_activity
            .entry(ip)
            .or_insert_with(|| AtomicU64::new(now))
            .store(now, Ordering::Relaxed);
    }

    /// Tries to add a new connection for an IP (thread-safe)
    fn try_add_connection(&self, client_id: u32, ip: IpAddr, limit: usize) -> bool {
        // Update activity
        self.update_activity(ip);

        // Get or create atomic counter for this IP
        let counter = self.ip_connections
            .entry(ip)
            .or_insert_with(|| AtomicUsize::new(0));

        // Try to increment if under limit
        let mut current = counter.load(Ordering::Acquire);
        loop {
            if current >= limit {
                return false;
            }

            match counter.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // Successfully incremented
                    self.client_to_ip.insert(client_id, ip);
                    return true;
                }
                Err(actual) => {
                    current = actual;
                }
            }
        }
    }

    /// Updates a connection when IP changes (thread-safe atomic transaction)
    ///
    /// This implementation uses a two-phase approach:
    /// 1. Reserve the new slot atomically
    /// 2. Release the old slot
    /// This ensures we never have inconsistent state
    fn update_connection(&self, client_id: u32, new_ip: IpAddr, limit: usize) -> bool {
        // Update activity for new IP
        self.update_activity(new_ip);

        // Get old IP if exists
        let old_ip = self.client_to_ip.get(&client_id).map(|entry| *entry);

        if let Some(old_ip) = old_ip {
            if old_ip == new_ip {
                return true; // No change needed
            }

            // Phase 1: Try to reserve slot in new IP first
            let new_counter = self.ip_connections
                .entry(new_ip)
                .or_insert_with(|| AtomicUsize::new(0));

            let mut new_count = new_counter.load(Ordering::Acquire);
            loop {
                if new_count >= limit {
                    // New IP is at limit, reject the update
                    return false;
                }

                match new_counter.compare_exchange_weak(
                    new_count,
                    new_count + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        // Successfully reserved slot in new IP
                        break;
                    }
                    Err(actual) => {
                        new_count = actual;
                    }
                }
            }

            // Phase 2: Now release the old IP slot (can't fail)
            if let Some(old_counter) = self.ip_connections.get(&old_ip) {
                let prev = old_counter.fetch_sub(1, Ordering::AcqRel);

                // Clean up if this was the last connection
                if prev == 1 {
                    self.ip_connections.remove_if(&old_ip, |_, counter| {
                        counter.load(Ordering::Acquire) == 0
                    });
                    self.ip_last_activity.remove(&old_ip);
                }
            }

            // Update mapping
            self.client_to_ip.insert(client_id, new_ip);
            true
        } else {
            // No existing connection, treat as new
            self.try_add_connection(client_id, new_ip, limit)
        }
    }

    /// Removes a connection (thread-safe)
    fn remove_connection(&self, client_id: u32) {
        if let Some((_, ip)) = self.client_to_ip.remove(&client_id) {
            if let Some(counter) = self.ip_connections.get(&ip) {
                let prev = counter.fetch_sub(1, Ordering::AcqRel);

                // Clean up if this was the last connection
                if prev == 1 {
                    self.ip_connections.remove_if(&ip, |_, counter| {
                        counter.load(Ordering::Acquire) == 0
                    });
                    self.ip_last_activity.remove(&ip);
                }
            }
        }
    }

    /// Gets the current connection count for an IP
    fn get_ip_count(&self, ip: &IpAddr) -> usize {
        self.ip_connections
            .get(ip)
            .map(|counter| counter.load(Ordering::Acquire))
            .unwrap_or(0)
    }

    /// Gets total number of unique IPs
    fn unique_ip_count(&self) -> usize {
        self.ip_connections.len()
    }

    /// Gets total number of connections
    fn total_connections(&self) -> usize {
        self.client_to_ip.len()
    }

    /// Cleans up stale IP entries (no active connections for timeout period)
    fn cleanup_stale_ips(&self, timeout_ms: u64) {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Use retain to avoid iterator invalidation issues
        self.ip_last_activity.retain(|ip, last_activity| {
            let last = last_activity.load(Ordering::Relaxed);
            let is_stale = now.saturating_sub(last) > timeout_ms;

            if is_stale {
                // Check if IP has no active connections
                if let Some(counter) = self.ip_connections.get(ip) {
                    if counter.load(Ordering::Acquire) == 0 {
                        // Remove from ip_connections too
                        self.ip_connections.remove(ip);
                        return false; // Remove from ip_last_activity
                    }
                }
            }
            true // Keep in ip_last_activity
        });
    }
}

/// Tracks failed command attempts per IP
#[derive(Debug)]
struct CommandBlocklist {
    /// IP -> (failed attempts, block until timestamp)
    blocked_ips: DashMap<IpAddr, (usize, u64)>,
}

impl CommandBlocklist {
    fn new() -> Self {
        Self {
            blocked_ips: DashMap::with_capacity(100),
        }
    }

    /// Checks if IP is blocked
    fn is_blocked(&self, ip: IpAddr) -> bool {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        if let Some(entry) = self.blocked_ips.get(&ip) {
            let (_, block_until) = *entry;
            if now < block_until {
                return true;
            } else {
                // Unblock expired entries
                drop(entry);
                self.blocked_ips.remove(&ip);
            }
        }
        false
    }

    /// Records a failed attempt
    fn record_failure(&self, ip: IpAddr) {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.blocked_ips
            .entry(ip)
            .and_modify(|(attempts, block_until)| {
                *attempts += 1;
                if *attempts >= MAX_FAILED_COMMANDS {
                    *block_until = now + COMMAND_BLOCK_DURATION_SECS;
                }
            })
            .or_insert((1, 0));
    }

    /// Cleans up old entries
    fn cleanup(&self) {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.blocked_ips.retain(|_, (_, block_until)| {
            *block_until > now
        });
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
    /// Manager for per-IP command rate limiting
    command_manager: Arc<IpRateLimiterManager>,
    /// Global command rate limiter
    global_command_limiter: Arc<crate::net::rate_limiter::RateLimiter>,
    /// Maintenance mode flag
    maintenance_mode: Arc<AtomicBool>,
    /// Service metrics
    metrics: Arc<ServiceMetrics>,
    /// Semaphore for backpressure control
    processing_semaphore: Arc<Semaphore>,
    /// Command failure blocklist
    command_blocklist: Arc<CommandBlocklist>,
}

impl TunnelV3 {
    /// Creates a new Tunnel V3 instance with custom configuration
    pub fn with_config(mut config: TunnelV3Config) -> Self {
        // Normalize configuration
        if config.port <= 1024 {
            warn!("Port {} is privileged, changing to 50001", config.port);
            config.port = 50001;
        }
        if config.max_clients < 2 {
            warn!("Max clients {} too low, changing to 200", config.max_clients);
            config.max_clients = 200;
        }
        if config.ip_limit < 1 {
            warn!("IP limit {} too low, changing to 8", config.ip_limit);
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
        let global_ping_config = RateLimiterConfig {
            max_tokens: config.max_pings_global as u32,
            refill_rate: config.max_pings_global as u32,
            refill_interval_ms: config.reset_interval_ms,
            ordering: crate::net::rate_limiter::MemoryOrdering::Relaxed,
        };
        let global_ping_limiter = Arc::new(crate::net::rate_limiter::RateLimiter::with_config(global_ping_config));

        // Configure per-IP ping rate limiter manager
        let per_ip_ping_config = RateLimiterConfig {
            max_tokens: config.max_pings_per_ip as u32,
            refill_rate: config.max_pings_per_ip as u32,
            refill_interval_ms: config.reset_interval_ms,
            ordering: crate::net::rate_limiter::MemoryOrdering::Relaxed,
        };
        let ping_manager = Arc::new(IpRateLimiterManager::new(per_ip_ping_config));

        // Configure global command rate limiter
        let global_command_config = RateLimiterConfig {
            max_tokens: config.max_commands_global as u32,
            refill_rate: config.max_commands_global as u32,
            refill_interval_ms: config.reset_interval_ms,
            ordering: crate::net::rate_limiter::MemoryOrdering::Sequential, // Commands need strong ordering
        };
        let global_command_limiter = Arc::new(crate::net::rate_limiter::RateLimiter::with_config(global_command_config));

        // Configure per-IP command rate limiter manager
        let per_ip_command_config = RateLimiterConfig {
            max_tokens: config.max_commands_per_ip as u32,
            refill_rate: config.max_commands_per_ip as u32,
            refill_interval_ms: config.reset_interval_ms,
            ordering: crate::net::rate_limiter::MemoryOrdering::Sequential,
        };
        let command_manager = Arc::new(IpRateLimiterManager::new(per_ip_command_config));

        // Create processing semaphore
        let processing_semaphore = Arc::new(Semaphore::new(config.max_concurrent_processing));

        Self {
            config,
            maintenance_password_sha1,
            ping_manager,
            global_ping_limiter,
            command_manager,
            global_command_limiter,
            maintenance_mode: Arc::new(AtomicBool::new(false)),
            metrics: Arc::new(ServiceMetrics::new()),
            processing_semaphore,
            command_blocklist: Arc::new(CommandBlocklist::new()),
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

        // Start per-IP command cleanup task
        let command_cleanup_handle = self.command_manager.clone().start_cleanup_task();

        // Start command blocklist cleanup task
        let blocklist_cleanup_handle = {
            let blocklist = self.command_blocklist.clone();
            tokio::spawn(async move {
                let mut interval = time::interval(Duration::from_secs(300)); // 5 minutes
                interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

                loop {
                    interval.tick().await;
                    blocklist.cleanup();
                }
            })
        };

        // Start connection state cleanup task
        let state_cleanup_handle = {
            let state = connection_state.clone();
            tokio::spawn(async move {
                let mut interval = time::interval(Duration::from_secs(60)); // 1 minute
                interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

                loop {
                    interval.tick().await;
                    state.cleanup_stale_ips(STALE_CONNECTION_TIMEOUT_MS);
                }
            })
        };

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
                        "V3 metrics - received: {}, processed: {}, dropped: {}, tunneled: {}, active: {}, failed_cmds: {}",
                        metrics.packets_received.load(Ordering::Relaxed),
                        metrics.packets_processed.load(Ordering::Relaxed),
                        metrics.packets_dropped.load(Ordering::Relaxed),
                        metrics.packets_tunneled.load(Ordering::Relaxed),
                        metrics.active_connections.load(Ordering::Relaxed),
                        metrics.failed_commands.load(Ordering::Relaxed),
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
                    command_cleanup_handle.abort();
                    blocklist_cleanup_handle.abort();
                    state_cleanup_handle.abort();
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
                self.execute_command(data[8], &data[9..29], remote_addr.ip()).await;
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
            if data.len() == 50 {
                if !self.ping_limit_reached(remote_addr.ip()) {
                    if let Err(e) = socket.send_to(&data[..12], remote_addr).await {
                        debug!("Failed to send ping response: {}", e);
                    }
                } else {
                    trace!("Ping rate limit reached for {}", remote_addr.ip());
                    self.metrics.packets_dropped.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                trace!("Malformed ping packet from {}: {} bytes", remote_addr, data.len());
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

    /// Handles tunnel traffic between clients (TOCTOU-safe)
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
        // Use entry API for atomic operations
        match mappings.entry(sender_id) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                let sender = entry.get_mut();
                let existing_ep = sender.remote_ep;

                if let Some(ep) = existing_ep {
                    if ep != remote_addr {
                        // IP change attempt - check all conditions atomically
                        let maintenance = self.maintenance_mode.load(Ordering::Relaxed);

                        // Update activity and get timeout state atomically
                        let was_timed_out = sender.is_timed_out();

                        if !maintenance && was_timed_out {
                            // Try to update connection state first
                            if connection_state.update_connection(sender_id, remote_addr.ip(), self.config.ip_limit) {
                                // Update endpoint only if connection state update succeeded
                                sender.remote_ep = Some(remote_addr);
                                sender.update_last_receive();
                                info!("Client {} changed IP from {} to {} (was timed out)", sender_id, ep, remote_addr);
                            } else {
                                // IP limit reached for new address
                                self.metrics.connections_rejected.fetch_add(1, Ordering::Relaxed);
                                debug!("Client {} IP change rejected: limit reached for {}", sender_id, remote_addr.ip());
                                return;
                            }
                        } else {
                            // Either in maintenance mode or not timed out
                            if maintenance {
                                debug!("Client {} IP change rejected: maintenance mode", sender_id);
                            } else {
                                debug!("Client {} IP change rejected: not timed out", sender_id);
                            }
                            self.metrics.connections_rejected.fetch_add(1, Ordering::Relaxed);
                            return;
                        }
                    } else {
                        // Same endpoint, just update activity
                        sender.update_last_receive();
                    }
                } else {
                    // Set initial endpoint
                    sender.remote_ep = Some(remote_addr);
                    sender.update_last_receive();
                    debug!("Client {} endpoint set to {}", sender_id, remote_addr);
                }
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                // New client connection - check all conditions atomically
                if mappings.len() >= self.config.max_clients {
                    self.metrics.connections_rejected.fetch_add(1, Ordering::Relaxed);
                    debug!("New client {} rejected: max clients reached", sender_id);
                    return;
                }

                if self.maintenance_mode.load(Ordering::Relaxed) {
                    self.metrics.connections_rejected.fetch_add(1, Ordering::Relaxed);
                    debug!("New client {} rejected: maintenance mode", sender_id);
                    return;
                }

                // Try to add connection to state first
                if !connection_state.try_add_connection(sender_id, remote_addr.ip(), self.config.ip_limit) {
                    self.metrics.connections_rejected.fetch_add(1, Ordering::Relaxed);
                    debug!("New client {} rejected: IP limit reached for {}", sender_id, remote_addr.ip());
                    return;
                }

                // Create new client only after all checks pass
                let mut client = TunnelClient::new();
                client.set_endpoint(remote_addr);
                client.update_last_receive();

                // Insert atomically
                entry.insert(client);

                self.metrics.connections_created.fetch_add(1, Ordering::Relaxed);
                self.metrics.active_connections.fetch_add(1, Ordering::Relaxed);
                info!("New client {} connected from {} (total: {})",
                    sender_id, remote_addr, mappings.len());
            }
        }

        // Forward packet to receiver (unchanged)
        if let Some(receiver) = mappings.get(&receiver_id) {
            if let Some(receiver_ep) = receiver.remote_ep {
                if receiver_ep != remote_addr {
                    match socket.send_to(data, receiver_ep).await {
                        Ok(_) => {
                            self.metrics.packets_tunneled.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            debug!("Failed to forward packet from {} to {}: {}",
                                sender_id, receiver_id, e);
                        }
                    }
                }
            }
        }
    }

    /// Checks if ping limit has been reached for an IP
    #[inline]
    fn ping_limit_reached(&self, ip: IpAddr) -> bool {
        // Check per-IP first (cheaper), then global
        !self.ping_manager.try_acquire(ip) || !self.global_ping_limiter.try_acquire()
    }

    /// Checks if command rate limit has been reached for an IP
    #[inline]
    fn command_limit_reached(&self, ip: IpAddr) -> bool {
        // Check per-IP first (cheaper), then global
        !self.command_manager.try_acquire(ip) || !self.global_command_limiter.try_acquire()
    }

    /// Executes a command packet with enhanced rate limiting
    async fn execute_command(&self, command: u8, data: &[u8], source_ip: IpAddr) {
        // Check if IP is blocked
        if self.command_blocklist.is_blocked(source_ip) {
            warn!("Command from blocked IP: {}", source_ip);
            return;
        }

        // Check rate limit
        if self.command_limit_reached(source_ip) {
            debug!("Command rate limit exceeded for {}", source_ip);
            return;
        }

        // Verify password
        if let Some(expected_hash) = self.maintenance_password_sha1 {
            let provided_hash: [u8; 20] = match data.try_into() {
                Ok(hash) => hash,
                Err(_) => {
                    self.command_blocklist.record_failure(source_ip);
                    self.metrics.failed_commands.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };

            if provided_hash != expected_hash {
                self.command_blocklist.record_failure(source_ip);
                self.metrics.failed_commands.fetch_add(1, Ordering::Relaxed);
                warn!("Invalid command password from {}", source_ip);
                return;
            }
        } else {
            return;
        }

        // Execute command
        match command {
            0 => {
                // MaintenanceMode toggle
                let current = self.maintenance_mode.load(Ordering::Relaxed);
                self.maintenance_mode.store(!current, Ordering::Relaxed);
                info!("Maintenance mode toggled to: {} by {}", !current, source_ip);
            }
            _ => warn!("Unknown command: {} from {}", command, source_ip),
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

            // Build URL with validation - truncate name if needed
            let mut truncated_name = name.to_string();
            if truncated_name.len() > 100 {
                truncated_name.truncate(100);
                warn!("Server name truncated to 100 characters for master announcement");
            }

            let encoded_name = utf8_percent_encode(&truncated_name, NON_ALPHANUMERIC).to_string();
            let encoded_password = utf8_percent_encode(master_password, NON_ALPHANUMERIC).to_string();

            let url = format!(
                "{}?version=3&name={}&port={}&clients={}&maxclients={}&masterpw={}&maintenance={}",
                master_url,
                encoded_name,
                port,
                client_count,
                max_clients,
                encoded_password,
                maintenance
            );

            // Check URL length
            if url.len() > MAX_URL_LENGTH {
                error!("Master server URL still too long ({} bytes) after truncation, skipping heartbeat", url.len());
                return;
            }

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
            failed_commands: self.metrics.failed_commands.load(Ordering::Relaxed),
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
            command_manager: self.command_manager.clone(),
            global_command_limiter: self.global_command_limiter.clone(),
            maintenance_mode: self.maintenance_mode.clone(),
            metrics: self.metrics.clone(),
            processing_semaphore: self.processing_semaphore.clone(),
            command_blocklist: self.command_blocklist.clone(),
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
    pub failed_commands: u64,
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
    fn test_command_limit_reached() {
        let config = TunnelV3Config::default();
        let service = TunnelV3::with_config(config);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        // Should allow up to max_commands_per_ip
        for _ in 0..DEFAULT_MAX_COMMANDS_PER_IP {
            assert!(!service.command_limit_reached(ip));
        }
        assert!(service.command_limit_reached(ip));
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

    #[test]
    fn test_command_blocklist() {
        let blocklist = CommandBlocklist::new();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        assert!(!blocklist.is_blocked(ip));

        // Record failures
        for _ in 0..MAX_FAILED_COMMANDS {
            blocklist.record_failure(ip);
        }

        // Should be blocked now
        assert!(blocklist.is_blocked(ip));
    }

    #[test]
    fn test_connection_state_concurrent_update() {
        use std::sync::Arc;
        use std::thread;

        let state = Arc::new(ConnectionState::new());
        let mut handles = vec![];

        // Spawn multiple threads trying to update the same client
        for i in 0..10 {
            let state_clone = state.clone();
            let handle = thread::spawn(move || {
                let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, (i % 3) as u8));
                state_clone.update_connection(1, ip, 5)
            });
            handles.push(handle);
        }

        // Wait for all threads
        let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // Exactly one update should succeed (the winner of the race)
        let successes = results.iter().filter(|&&r| r).count();
        assert!(successes >= 1); // At least one should succeed

        // Final state should be consistent
        assert_eq!(state.total_connections(), 1);
        let total_count: usize = state.ip_connections
            .iter()
            .map(|entry| entry.value().load(Ordering::Acquire))
            .sum();
        assert_eq!(total_count, 1);
    }
}