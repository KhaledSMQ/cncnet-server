//! Tunnel V2 implementation
//!
//! This module implements the V2 tunnel protocol for CnCNet.
//! It provides both UDP tunneling and HTTP-based game requests.
//!
//! ## Protocol Overview
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────────────────┐
//! │                         Tunnel V2 Architecture                         │
//! ├────────────────────────────────────────────────────────────────────────┤
//! │                                                                        │
//! │  Game Client A                                    Game Client B        │
//! │       │                                                 │              │
//! │       ├──────[UDP Tunnel Traffic]────────┐              │              │
//! │       │                                  ▼              │              │
//! │       │                          ┌───────────────┐      │              │
//! │       │                          │  UDP Socket   │      │              │
//! │       │                          │   :50000      │      │              │
//! │       │                          └───────┬───────┘      │              │
//! │       │                                  │              │              │
//! │       │                          ┌───────▼───────┐      │              │
//! │       │                          │ Packet Router │      │              │
//! │       │                          └───────┬───────┘      │              │
//! │       │                                  │              │              │
//! │       │                                  └──────────────┴──────────────┤
//! │       │                             [Forward to Client B]              │
//! │       │                                                                │
//! │       │   [HTTP Game Request]                                          │
//! │       └────────────┐                                                   │
//! │                    ▼                                                   │
//! │            ┌───────────────┐                                           │
//! │            │  HTTP Server  │                                           │
//! │            │   :50000      │                                           │
//! │            └───────┬───────┘                                           │
//! │                    │                                                   │
//! │         ┌──────────┴──────────┐                                        │
//! │         ▼                     ▼                                        │
//! │   ┌───────────┐         ┌───────────┐                                  │
//! │   │  /status  │         │ /request  │                                  │
//! │   │           │         │           │                                  │
//! │   │ Show free │         │ Allocate  │                                  │
//! │   │   slots   │         │ game IDs  │                                  │
//! │   └───────────┘         └───────────┘                                  │
//! │                                                                        │
//! │  ┌────────────────────────────────────────────────────────────────┐    │
//! │  │                        UDP Packet Format                       │    │
//! │  ├────────────────────────────────────────────────────────────────┤    │
//! │  │  0-1:  Sender ID (i16 BE)                                      │    │
//! │  │  2-3:  Receiver ID (i16 BE)                                    │    │
//! │  │  4+:   Payload Data                                            │    │
//! │  │                                                                │    │
//! │  │  Special IDs:                                                  │    │
//! │  │  - 0,0: Ping packet (50 bytes total)                           │    │
//! │  └────────────────────────────────────────────────────────────────┘    │
//! │                                                                        │
//! │  ┌────────────────────────────────────────────────────────────────┐    │
//! │  │                    HTTP Endpoints                              │    │
//! │  ├────────────────────────────────────────────────────────────────┤    │
//! │  │                                                                │    │
//! │  │  GET /status                                                   │    │
//! │  │    Response: "X slots free.\nY slots in use.\n"                │    │
//! │  │                                                                │    │
//! │  │  GET /request?clients=N                                        │    │
//! │  │    Allocates N client IDs (2-8)                                │    │
//! │  │    Response: "[id1,id2,...,idN]"                               │    │
//! │  │                                                                │    │
//! │  │  GET /maintenance/<password>                                   │    │
//! │  │    Enables maintenance mode                                    │    │
//! │  │                                                                │    │
//! │  └────────────────────────────────────────────────────────────────┘    │
//! │                                                                        │
//! │  ┌────────────────────────────────────────────────────────────────┐    │
//! │  │                    Master Server Protocol                      │    │
//! │  ├────────────────────────────────────────────────────────────────┤    │
//! │  │                                                                │    │
//! │  │  Heartbeat every 60 seconds:                                   │    │
//! │  │  GET /master-announce?version=2&name=...&port=...&clients=...  │    │
//! │  │                                                                │    │
//! │  └────────────────────────────────────────────────────────────────┘    │
//! └────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! Key features:
//! - UDP socket for game traffic tunneling
//! - HTTP server for game slot allocation
//! - Master server heartbeat announcements
//! - Per-IP rate limiting for requests and pings
//! - Graceful shutdown support
//! - Enhanced logging and error handling
//! - Low memory usage with automatic cleanup
//! - Bounded concurrent HTTP connections
//! - Connection pooling for master server requests
//! - Efficient client ID allocation with free list

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use dashmap::DashMap;
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use once_cell::sync::Lazy;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use rand::Rng;
use reqwest;
use smallvec::SmallVec;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::{oneshot::{self, Receiver}, Semaphore};
use tokio::time::{self, timeout};
use tracing::{debug, error, info, warn};

use crate::net::constants::*;
use crate::net::rate_limiter::{IpRateLimiterManager, RateLimiterConfig};
use crate::net::tunnel_client::TunnelClient;
use crate::net::utils::create_optimized_socket;

/// Maximum concurrent HTTP connections
const MAX_CONCURRENT_HTTP: usize = 100;

/// Maximum concurrent packet processing tasks
const MAX_CONCURRENT_PROCESSING: usize = 2000;

/// Timeout for UDP operations
const UDP_OPERATION_TIMEOUT: Duration = Duration::from_millis(100);

/// Maximum packet size we'll accept
const MAX_PACKET_SIZE: usize = 1024;

/// Minimum valid packet size
const MIN_PACKET_SIZE: usize = 4;

/// Default maximum pings per IP during one interval
const DEFAULT_MAX_PINGS_PER_IP: usize = 20;

/// Default maximum total pings globally during one interval
const DEFAULT_MAX_PINGS_GLOBAL: usize = 5000;

/// Default maximum requests per IP during one interval
const DEFAULT_MAX_REQUESTS_PER_IP: usize = 4;

/// Default maximum total requests globally during one interval
const DEFAULT_MAX_REQUESTS_GLOBAL: usize = 1000;

/// Default counter reset interval in milliseconds
const DEFAULT_RESET_INTERVAL_MS: u64 = 60_000;

/// Maximum URL length for HTTP requests
const MAX_URL_LENGTH: usize = 2000;

/// Maximum attempts to find a free client ID
const MAX_ID_ALLOCATION_ATTEMPTS: usize = 50;

/// HTTP client for master server announcements with fallback
static HTTP_CLIENT: Lazy<Arc<Mutex<Option<reqwest::Client>>>> = Lazy::new(|| {
    Arc::new(Mutex::new(create_http_client()))
});

/// Creates HTTP client with error handling
fn create_http_client() -> Option<reqwest::Client> {
    match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(1)
        .user_agent("CnCNet-Rust-Server/1.0")
        .build()
    {
        Ok(client) => {
            info!("HTTP client created successfully");
            Some(client)
        }
        Err(e) => {
            error!("Failed to create HTTP client: {}", e);
            error!("Master server announcements will be disabled");
            None
        }
    }
}

/// Gets HTTP client or attempts to recreate it
fn get_http_client() -> Option<reqwest::Client> {
    let mut client_opt = HTTP_CLIENT.lock().unwrap();

    if client_opt.is_none() {
        // Try to recreate client
        *client_opt = create_http_client();
    }

    client_opt.clone()
}

/// Configuration for the Tunnel V2 service
#[derive(Debug, Clone)]
pub struct TunnelV2Config {
    /// Port used for the tunnel server (UDP + HTTP)
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
    /// Maximum requests per IP during one reset interval
    pub max_requests_per_ip: usize,
    /// Maximum total requests globally during one reset interval
    pub max_requests_global: usize,
    /// Interval for resetting rate limiters (milliseconds)
    pub reset_interval_ms: u64,
    /// Maximum concurrent HTTP connections
    pub max_concurrent_http: usize,
    /// Maximum concurrent packet processing
    pub max_concurrent_processing: usize,
}

impl Default for TunnelV2Config {
    fn default() -> Self {
        Self {
            port: 50000,
            max_clients: 200,
            ip_limit: 4,
            name: "Unnamed server".to_string(),
            no_master_announce: false,
            master_password: String::new(),
            maintenance_password: "KUYn3b2z".to_string(),
            master_server_url: "http://cncnet.org/master-announce".to_string(),
            max_pings_per_ip: DEFAULT_MAX_PINGS_PER_IP,
            max_pings_global: DEFAULT_MAX_PINGS_GLOBAL,
            max_requests_per_ip: DEFAULT_MAX_REQUESTS_PER_IP,
            max_requests_global: DEFAULT_MAX_REQUESTS_GLOBAL,
            reset_interval_ms: DEFAULT_RESET_INTERVAL_MS,
            max_concurrent_http: MAX_CONCURRENT_HTTP,
            max_concurrent_processing: MAX_CONCURRENT_PROCESSING,
        }
    }
}

/// Service metrics for monitoring with proper memory ordering
#[derive(Debug)]
pub struct ServiceMetrics {
    /// Total UDP packets received (relaxed - counter only)
    udp_packets_received: AtomicU64,
    /// Total UDP packets processed (relaxed - counter only)
    udp_packets_processed: AtomicU64,
    /// Total UDP packets dropped (relaxed - counter only)
    udp_packets_dropped: AtomicU64,
    /// Total packets tunneled (relaxed - counter only)
    packets_tunneled: AtomicU64,
    /// Total HTTP requests received (relaxed - counter only)
    http_requests_received: AtomicU64,
    /// Total HTTP requests processed (relaxed - counter only)
    http_requests_processed: AtomicU64,
    /// Total HTTP requests rate limited (relaxed - counter only)
    http_requests_rate_limited: AtomicU64,
    /// Total game slots allocated (relaxed - counter only)
    game_slots_allocated: AtomicU64,
    /// Current active connections (AcqRel - needs consistency)
    active_connections: AtomicUsize,
}

impl ServiceMetrics {
    pub fn new() -> Self {
        Self {
            udp_packets_received: AtomicU64::new(0),
            udp_packets_processed: AtomicU64::new(0),
            udp_packets_dropped: AtomicU64::new(0),
            packets_tunneled: AtomicU64::new(0),
            http_requests_received: AtomicU64::new(0),
            http_requests_processed: AtomicU64::new(0),
            http_requests_rate_limited: AtomicU64::new(0),
            game_slots_allocated: AtomicU64::new(0),
            active_connections: AtomicUsize::new(0),
        }
    }

    // Counter increment methods (relaxed ordering is fine)
    #[inline]
    pub fn inc_udp_received(&self) {
        self.udp_packets_received.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_udp_processed(&self) {
        self.udp_packets_processed.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_udp_dropped(&self) {
        self.udp_packets_dropped.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_tunneled(&self) {
        self.packets_tunneled.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_http_received(&self) {
        self.http_requests_received.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_http_processed(&self) {
        self.http_requests_processed.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_http_rate_limited(&self) {
        self.http_requests_rate_limited.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn add_game_slots(&self, count: u64) {
        self.game_slots_allocated.fetch_add(count, Ordering::Relaxed);
    }

    // Active connections need stronger ordering for consistency
    #[inline]
    pub fn add_active_connections(&self, count: usize) {
        self.active_connections.fetch_add(count, Ordering::AcqRel);
    }

    #[inline]
    pub fn sub_active_connections(&self, count: usize) {
        self.active_connections.fetch_sub(count, Ordering::AcqRel);
    }

    #[inline]
    pub fn get_active_connections(&self) -> usize {
        self.active_connections.load(Ordering::Acquire)
    }

    /// Get a consistent snapshot of all metrics
    pub fn snapshot(&self) -> MetricsSnapshot {
        // Use Acquire ordering to ensure we see all updates
        MetricsSnapshot {
            udp_packets_received: self.udp_packets_received.load(Ordering::Acquire),
            udp_packets_processed: self.udp_packets_processed.load(Ordering::Acquire),
            udp_packets_dropped: self.udp_packets_dropped.load(Ordering::Acquire),
            packets_tunneled: self.packets_tunneled.load(Ordering::Acquire),
            http_requests_received: self.http_requests_received.load(Ordering::Acquire),
            http_requests_processed: self.http_requests_processed.load(Ordering::Acquire),
            http_requests_rate_limited: self.http_requests_rate_limited.load(Ordering::Acquire),
            game_slots_allocated: self.game_slots_allocated.load(Ordering::Acquire),
            active_connections: self.active_connections.load(Ordering::Acquire),
        }
    }
}

/// Immutable snapshot of metrics
#[derive(Debug, Clone, Copy)]
pub struct MetricsSnapshot {
    pub udp_packets_received: u64,
    pub udp_packets_processed: u64,
    pub udp_packets_dropped: u64,
    pub packets_tunneled: u64,
    pub http_requests_received: u64,
    pub http_requests_processed: u64,
    pub http_requests_rate_limited: u64,
    pub game_slots_allocated: u64,
    pub active_connections: usize,
}

/// Client ID allocator using a free list for efficiency
#[derive(Debug)]
struct ClientIdAllocator {
    /// Free list of available IDs
    free_ids: Mutex<VecDeque<i16>>,
    /// Next sequential ID to generate
    next_id: AtomicU64,
}

impl ClientIdAllocator {
    fn new() -> Self {
        Self {
            free_ids: Mutex::new(VecDeque::with_capacity(1000)),
            next_id: AtomicU64::new(1), // Start at 1, skip 0
        }
    }

    /// Allocates a new client ID or reuses a freed one
    fn allocate(&self) -> Option<i16> {
        // First check free list
        if let Ok(mut free_list) = self.free_ids.lock() {
            if let Some(id) = free_list.pop_front() {
                return Some(id);
            }
        }

        // Generate new sequential ID
        let next = self.next_id.fetch_add(1, Ordering::Relaxed);

        // Check if we've exhausted the i16 range
        if next > i16::MAX as u64 {
            // Reset counter and clear free list to start fresh
            self.next_id.store(1, Ordering::Relaxed);
            if let Ok(mut free_list) = self.free_ids.lock() {
                free_list.clear();
            }
            return self.allocate(); // Recursive retry
        }

        Some(next as i16)
    }

    /// Returns an ID to the free list for reuse
    fn free(&self, id: i16) {
        if id != 0 { // Don't add 0 to free list
            if let Ok(mut free_list) = self.free_ids.lock() {
                // Limit free list size to prevent unbounded growth
                if free_list.len() < 10000 {
                    free_list.push_back(id);
                }
            }
        }
    }
}

/// Tunnel V2 server implementation
pub struct TunnelV2 {
    /// Service configuration
    config: TunnelV2Config,
    /// Manager for per-IP ping rate limiting
    ping_manager: Arc<IpRateLimiterManager>,
    /// Global ping rate limiter
    global_ping_limiter: Arc<crate::net::rate_limiter::RateLimiter>,
    /// Manager for per-IP request rate limiting (HTTP)
    request_manager: Arc<IpRateLimiterManager>,
    /// Global request rate limiter (HTTP)
    global_request_limiter: Arc<crate::net::rate_limiter::RateLimiter>,
    /// Maintenance mode flag
    maintenance_mode: Arc<AtomicBool>,
    /// Service metrics
    metrics: Arc<ServiceMetrics>,
    /// Semaphore for HTTP connection limiting
    http_semaphore: Arc<Semaphore>,
    /// Semaphore for UDP packet processing
    processing_semaphore: Arc<Semaphore>,
    /// Client ID allocator
    id_allocator: Arc<ClientIdAllocator>,
}

impl TunnelV2 {
    /// Creates a new Tunnel V2 instance with custom configuration
    pub fn with_config(mut config: TunnelV2Config) -> Self {
        // Normalize configuration
        if config.port <= 1024 {
            warn!("Port {} is privileged, changing to 50000", config.port);
            config.port = 50000;
        }
        if config.max_clients < 2 {
            warn!("Max clients {} too low, changing to 200", config.max_clients);
            config.max_clients = 200;
        }
        if config.ip_limit < 1 {
            warn!("IP limit {} too low, changing to 4", config.ip_limit);
            config.ip_limit = 4;
        }
        if config.name.is_empty() {
            config.name = "Unnamed server".to_string();
        } else {
            config.name = config.name.replace(';', "");
        }

        // Configure global ping rate limiter
        let global_ping_config = RateLimiterConfig {
            max_tokens: config.max_pings_global as u32,
            refill_rate: config.max_pings_global as u32,
            refill_interval_ms: config.reset_interval_ms,
            ordering: crate::net::rate_limiter::MemoryOrdering::Relaxed,
        };
        let global_ping_limiter = Arc::new(crate::net::rate_limiter::RateLimiter::with_config(global_ping_config));

        // Configure per-IP ping rate limiter
        let per_ip_ping_config = RateLimiterConfig {
            max_tokens: config.max_pings_per_ip as u32,
            refill_rate: config.max_pings_per_ip as u32,
            refill_interval_ms: config.reset_interval_ms,
            ordering: crate::net::rate_limiter::MemoryOrdering::Relaxed,
        };
        let ping_manager = Arc::new(IpRateLimiterManager::new(per_ip_ping_config));

        // Configure global request rate limiter
        let global_request_config = RateLimiterConfig {
            max_tokens: config.max_requests_global as u32,
            refill_rate: config.max_requests_global as u32,
            refill_interval_ms: config.reset_interval_ms,
            ordering: crate::net::rate_limiter::MemoryOrdering::Relaxed,
        };
        let global_request_limiter = Arc::new(crate::net::rate_limiter::RateLimiter::with_config(global_request_config));

        // Configure per-IP request rate limiter
        let per_ip_request_config = RateLimiterConfig {
            max_tokens: config.max_requests_per_ip as u32,
            refill_rate: config.max_requests_per_ip as u32,
            refill_interval_ms: config.reset_interval_ms,
            ordering: crate::net::rate_limiter::MemoryOrdering::Relaxed,
        };
        let request_manager = Arc::new(IpRateLimiterManager::new(per_ip_request_config));

        // Create semaphores for concurrency control
        let http_semaphore = Arc::new(Semaphore::new(config.max_concurrent_http));
        let processing_semaphore = Arc::new(Semaphore::new(config.max_concurrent_processing));

        Self {
            config,
            ping_manager,
            global_ping_limiter,
            request_manager,
            global_request_limiter,
            maintenance_mode: Arc::new(AtomicBool::new(false)),
            metrics: Arc::new(ServiceMetrics::new()),
            http_semaphore,
            processing_semaphore,
            id_allocator: Arc::new(ClientIdAllocator::new()),
        }
    }

    /// Creates a new Tunnel V2 instance with default configuration
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
        let config = TunnelV2Config {
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

    /// Starts the Tunnel V2 service
    ///
    /// Returns a sender for shutdown signaling and the task join handle
    pub async fn start(
        self: Arc<Self>,
    ) -> Result<(oneshot::Sender<()>, tokio::task::JoinHandle<Result<()>>)> {
        info!("Starting Tunnel V2 with config: {:?}", self.config);

        // Create optimized UDP socket
        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), self.config.port);
        let udp_socket = Arc::new(create_optimized_socket(socket_addr).await?);

        // Bind TCP for HTTP
        let tcp_listener = TcpListener::bind(socket_addr).await?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        // Spawn the main service task
        let handle = tokio::spawn(self.run(udp_socket, tcp_listener, shutdown_rx));

        Ok((shutdown_tx, handle))
    }

    /// Internal run loop for the service
    async fn run(
        self: Arc<Self>,
        udp_socket: Arc<UdpSocket>,
        tcp_listener: TcpListener,
        mut shutdown_rx: Receiver<()>,
    ) -> Result<()> {
        // Client mappings (ID to client)
        let mappings = Arc::new(DashMap::<i16, TunnelClient>::with_capacity(self.config.max_clients));

        // Start cleanup tasks for rate limiters
        let ping_cleanup_handle = self.ping_manager.clone().start_cleanup_task();
        let request_cleanup_handle = self.request_manager.clone().start_cleanup_task();

        // Start heartbeat task
        let heartbeat_handle = {
            let mappings = mappings.clone();
            let maintenance_mode = self.maintenance_mode.clone();
            let no_master = self.config.no_master_announce;
            let url = self.config.master_server_url.clone();
            let name = self.config.name.clone();
            let port = self.config.port;
            let max_clients = self.config.max_clients;
            let master_pw = self.config.master_password.clone();
            let metrics = self.metrics.clone();
            let id_allocator = self.id_allocator.clone();

            tokio::spawn(async move {
                let mut interval = time::interval(Duration::from_millis(MASTER_ANNOUNCE_INTERVAL));
                interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

                loop {
                    interval.tick().await;
                    Self::send_heartbeat(
                        &mappings,
                        &maintenance_mode,
                        no_master,
                        &url,
                        &name,
                        port,
                        max_clients,
                        &master_pw,
                        &metrics,
                        &id_allocator,
                    )
                        .await;
                }
            })
        };

        // Send initial heartbeat
        Self::send_heartbeat(
            &mappings,
            &self.maintenance_mode,
            self.config.no_master_announce,
            &self.config.master_server_url,
            &self.config.name,
            self.config.port,
            self.config.max_clients,
            &self.config.master_password,
            &self.metrics,
            &self.id_allocator,
        )
            .await;

        // Start HTTP handling task
        let http_handle = {
            let service = self.clone();
            let mappings = mappings.clone();
            let listener = tcp_listener;

            tokio::spawn(async move {
                loop {
                    // Try to acquire HTTP connection permit
                    let permit = match service.http_semaphore.clone().acquire_owned().await {
                        Ok(permit) => permit,
                        Err(_) => break, // Semaphore closed, shutting down
                    };

                    match listener.accept().await {
                        Ok((stream, remote_addr)) => {
                            let service = service.clone();
                            let mappings = mappings.clone();

                            tokio::spawn(async move {
                                let _permit = permit; // Hold permit until done

                                let io = TokioIo::new(stream);
                                let service_fn = service_fn(move |req| {
                                    let service = service.clone();
                                    let mappings = mappings.clone();
                                    async move {
                                        service.handle_http_request(req, remote_addr, mappings).await
                                    }
                                });

                                if let Err(e) = http1::Builder::new()
                                    .serve_connection(io, service_fn)
                                    .await
                                {
                                    debug!("HTTP connection error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            error!("TCP accept error: {}", e);
                        }
                    }
                }
            })
        };

        // Start metrics reporting task
        let metrics_handle = {
            let metrics = self.metrics.clone();
            tokio::spawn(async move {
                let mut interval = time::interval(Duration::from_secs(60));
                interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

                loop {
                    interval.tick().await;
                    let snapshot = metrics.snapshot();
                    info!(
                        "V2 metrics - UDP recv: {}, proc: {}, drop: {}, tunnel: {}, HTTP recv: {}, proc: {}, rate_lim: {}, slots: {}, active: {}",
                        snapshot.udp_packets_received,
                        snapshot.udp_packets_processed,
                        snapshot.udp_packets_dropped,
                        snapshot.packets_tunneled,
                        snapshot.http_requests_received,
                        snapshot.http_requests_processed,
                        snapshot.http_requests_rate_limited,
                        snapshot.game_slots_allocated,
                        snapshot.active_connections,
                    );
                }
            })
        };

        // Pre-allocate receive buffer
        let mut recv_buf = vec![0u8; MAX_PACKET_SIZE];

        // Main UDP receive loop
        loop {
            tokio::select! {
                // Handle shutdown signal
                _ = &mut shutdown_rx => {
                    info!("Tunnel V2 shutting down");
                    ping_cleanup_handle.abort();
                    request_cleanup_handle.abort();
                    heartbeat_handle.abort();
                    http_handle.abort();
                    metrics_handle.abort();
                    break Ok(());
                }

                // Receive UDP packets with timeout
                result = timeout(UDP_OPERATION_TIMEOUT, udp_socket.recv_from(&mut recv_buf)) => {
                    match result {
                        Ok(Ok((size, addr))) => {
                            self.metrics.inc_udp_received();

                            // Basic validation
                            if size < MIN_PACKET_SIZE || size > MAX_PACKET_SIZE {
                                self.metrics.inc_udp_dropped();
                                continue;
                            }

                            // Try to acquire processing permit (backpressure)
                            let permit = match self.processing_semaphore.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    self.metrics.inc_udp_dropped();
                                    warn!("V2 processing queue full, dropping packet");
                                    continue;
                                }
                            };

                            // Clone data for async processing
                            let data: SmallVec<[u8; 64]> = recv_buf[..size].into();
                            let socket = udp_socket.clone();
                            let service = self.clone();
                            let mappings = mappings.clone();

                            // Spawn task to handle packet
                            tokio::spawn(async move {
                                let _permit = permit; // Hold permit
                                service.on_receive(&data, addr, &socket, &mappings).await;
                            });
                        }
                        Ok(Err(e)) => {
                            error!("V2 UDP receive error: {}", e);
                        }
                        Err(_) => {
                            // Timeout - normal, continue
                        }
                    }
                }
            }
        }
    }

    /// Handles a received UDP packet
    async fn on_receive(
        &self,
        data: &[u8],
        remote_addr: SocketAddr,
        socket: &UdpSocket,
        mappings: &DashMap<i16, TunnelClient>,
    ) {
        let sender_id = i16::from_be_bytes([data[0], data[1]]);
        let receiver_id = i16::from_be_bytes([data[2], data[3]]);

        // Validate packet
        if !self.is_valid_packet(sender_id, receiver_id, &remote_addr) {
            self.metrics.inc_udp_dropped();
            return;
        }

        // Handle ping packets
        if sender_id == 0 && receiver_id == 0 {
            if data.len() == 50 && !self.ping_limit_reached(remote_addr.ip()) {
                if let Err(e) = socket.send_to(&data[..12], remote_addr).await {
                    debug!("Failed to send ping response: {}", e);
                }
            }
            self.metrics.inc_udp_processed();
            return;
        }

        // Handle tunnel traffic
        if let Some(mut sender) = mappings.get_mut(&sender_id) {
            if sender.remote_ep.is_none() {
                sender.remote_ep = Some(remote_addr);
            } else if sender.remote_ep != Some(remote_addr) {
                return;
            }

            sender.update_last_receive();

            // Forward to receiver
            if let Some(receiver) = mappings.get(&receiver_id) {
                if let Some(receiver_ep) = receiver.remote_ep {
                    if receiver_ep != remote_addr {
                        if let Err(e) = socket.send_to(data, receiver_ep).await {
                            debug!("Failed to forward packet: {}", e);
                        } else {
                            self.metrics.inc_tunneled();
                        }
                    }
                }
            }
        }

        self.metrics.inc_udp_processed();
    }

    /// Validates packet parameters
    #[inline]
    pub fn is_valid_packet(&self, sender_id: i16, receiver_id: i16, addr: &SocketAddr) -> bool {
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

    /// Checks if ping limit has been reached for an IP
    #[inline]
    pub fn ping_limit_reached(&self, ip: IpAddr) -> bool {
        // Check per-IP first (cheaper), then global
        !self.ping_manager.try_acquire(ip) || !self.global_ping_limiter.try_acquire()
    }

    /// Handles HTTP requests
    async fn handle_http_request(
        &self,
        req: Request<hyper::body::Incoming>,
        remote_addr: SocketAddr,
        mappings: Arc<DashMap<i16, TunnelClient>>,
    ) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
        self.metrics.inc_http_received();

        let path = req.uri().path();

        // Basic path validation first (before rate limiting)
        let valid_path = path == "/status"
            || path == "/request"
            || path.starts_with("/maintenance/");

        if !valid_path {
            debug!("Invalid HTTP path: {} from {}", path, remote_addr);
            return Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::new()))
                .unwrap());
        }

        // Check rate limit only for valid paths
        if !self.request_manager.try_acquire(remote_addr.ip()) || !self.global_request_limiter.try_acquire() {
            self.metrics.inc_http_rate_limited();
            debug!("Rate limit exceeded for IP: {}", remote_addr.ip());
            return Ok(Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .body(Full::new(Bytes::new()))
                .unwrap());
        }

        let response = if path.starts_with("/maintenance/") {
            self.handle_maintenance_request(path, remote_addr)
        } else if path == "/status" {
            self.handle_status_request(&mappings)
        } else if path == "/request" {
            self.handle_game_request(req, &mappings).await
        } else {
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::new()))
                .unwrap()
        };

        self.metrics.inc_http_processed();
        Ok(response)
    }

    /// Handles maintenance mode requests
    fn handle_maintenance_request(&self, path: &str, remote_addr: SocketAddr) -> Response<Full<Bytes>> {
        if !self.config.maintenance_password.is_empty() {
            let parts: Vec<&str> = path.split('/').collect();
            if parts.len() > 2 && parts[2] == self.config.maintenance_password {
                self.maintenance_mode.store(true, Ordering::Relaxed);
                info!("Maintenance mode enabled via HTTP from {}", remote_addr);
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(Bytes::new()))
                    .unwrap();
            }
        }
        Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Full::new(Bytes::new()))
            .unwrap()
    }

    /// Handles status requests
    fn handle_status_request(&self, mappings: &DashMap<i16, TunnelClient>) -> Response<Full<Bytes>> {
        let active = mappings.len();
        let free = self.config.max_clients.saturating_sub(active);
        let status = format!(
            "{} slots free.\n{} slots in use.\n",
            free, active
        );
        Response::builder()
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from(status)))
            .unwrap()
    }

    /// Handles game slot allocation requests using a proper query parser
    async fn handle_game_request(
        &self,
        req: Request<hyper::body::Incoming>,
        mappings: &DashMap<i16, TunnelClient>,
    ) -> Response<Full<Bytes>> {
        if self.maintenance_mode.load(Ordering::Relaxed) {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Full::new(Bytes::new()))
                .unwrap();
        }

        // Parse query parameters properly
        let query = req.uri().query().unwrap_or("");
        let clients = Self::parse_clients_param(query);

        if clients < 2 || clients > 8 {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::new()))
                .unwrap();
        }

        // Allocate client IDs
        let client_ids = self.allocate_client_ids(&mappings, clients);

        if client_ids.len() < 2 {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Full::new(Bytes::new()))
                .unwrap();
        }

        self.metrics.add_game_slots(clients as u64);
        self.metrics.add_active_connections(clients);

        let response = format!(
            "[{}]",
            client_ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );

        info!("Allocated {} slots for game request", clients);

        Response::builder()
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from(response)))
            .unwrap()
    }

    /// Parses the clients parameter from query string
    fn parse_clients_param(query: &str) -> usize {
        // Handle multiple clients= parameters by taking the last one
        query
            .split('&')
            .filter_map(|param| {
                let mut parts = param.splitn(2, '=');
                match (parts.next(), parts.next()) {
                    (Some("clients"), Some(value)) => value.parse::<usize>().ok(),
                    _ => None,
                }
            })
            .last()
            .unwrap_or(0)
    }

    /// Allocates client IDs using the efficient allocator
    pub fn allocate_client_ids(
        &self,
        mappings: &DashMap<i16, TunnelClient>,
        count: usize,
    ) -> Vec<i16> {
        let mut client_ids = Vec::with_capacity(count);

        for _ in 0..count {
            let mut allocated = false;

            for _ in 0..MAX_ID_ALLOCATION_ATTEMPTS {
                // Check capacity before attempting allocation
                if mappings.len() >= self.config.max_clients {
                    // Rollback all allocated IDs
                    for id in &client_ids {
                        mappings.remove(id);
                        self.id_allocator.free(*id);
                    }
                    warn!("Max clients reached during allocation");
                    return vec![];
                }

                if let Some(id) = self.id_allocator.allocate() {
                    // Try to insert atomically
                    let client = TunnelClient::new();
                    client.update_last_receive();

                    // Use entry API for atomic check-and-insert
                    match mappings.entry(id) {
                        dashmap::mapref::entry::Entry::Vacant(entry) => {
                            entry.insert(client);
                            client_ids.push(id);
                            allocated = true;
                            break;
                        }
                        dashmap::mapref::entry::Entry::Occupied(_) => {
                            // ID already exists (shouldn't happen with allocator)
                            warn!("Allocated ID {} already exists in mappings", id);
                        }
                    }
                }
            }

            if !allocated {
                // Failed to allocate, rollback
                for id in &client_ids {
                    mappings.remove(id);
                    self.id_allocator.free(*id);
                }
                warn!("Failed to allocate ID after {} attempts", MAX_ID_ALLOCATION_ATTEMPTS);
                return vec![];
            }
        }

        debug!("Successfully allocated {} client IDs", client_ids.len());
        client_ids
    }

    /// Sends heartbeat to master server with better error handling
    async fn send_heartbeat(
        mappings: &DashMap<i16, TunnelClient>,
        maintenance_mode: &AtomicBool,
        no_master_announce: bool,
        master_url: &str,
        name: &str,
        port: u16,
        max_clients: usize,
        master_password: &str,
        metrics: &ServiceMetrics,
        id_allocator: &ClientIdAllocator,
    ) {
        // Clean up expired mappings and free their IDs
        let mut removed_count = 0;
        mappings.retain(|id, client| {
            if client.is_timed_out() {
                id_allocator.free(*id);
                removed_count += 1;
                false
            } else {
                true
            }
        });

        if removed_count > 0 {
            metrics.sub_active_connections(removed_count);
            debug!("Cleaned up {} timed out clients", removed_count);
        }

        let client_count = mappings.len();
        debug!("V2 Heartbeat: {} clients connected", client_count);

        // Send to master server if enabled
        if !no_master_announce {
            // Get HTTP client
            let client = match get_http_client() {
                Some(c) => c,
                None => {
                    debug!("HTTP client not available, skipping heartbeat");
                    return;
                }
            };

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
                "{}?version=2&name={}&port={}&clients={}&maxclients={}&masterpw={}&maintenance={}",
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
                error!("Master server URL too long ({} bytes), skipping heartbeat", url.len());
                return;
            }

            // Send with timeout and error handling
            let send_future = client.get(&url).send();
            match timeout(Duration::from_secs(10), send_future).await {
                Ok(Ok(response)) => {
                    if response.status().is_success() {
                        debug!("V2 Heartbeat sent successfully");
                    } else {
                        warn!("V2 Heartbeat failed with status: {}", response.status());
                    }
                }
                Ok(Err(e)) => {
                    error!("V2 Heartbeat request error: {}", e);
                }
                Err(_) => {
                    error!("V2 Heartbeat timeout after 10 seconds");
                }
            }
        }
    }

    /// Gets current service metrics
    pub fn metrics(&self) -> V2Metrics {
        let snapshot = self.metrics.snapshot();
        V2Metrics {
            udp_packets_received: snapshot.udp_packets_received,
            udp_packets_processed: snapshot.udp_packets_processed,
            udp_packets_dropped: snapshot.udp_packets_dropped,
            packets_tunneled: snapshot.packets_tunneled,
            http_requests_received: snapshot.http_requests_received,
            http_requests_processed: snapshot.http_requests_processed,
            http_requests_rate_limited: snapshot.http_requests_rate_limited,
            game_slots_allocated: snapshot.game_slots_allocated,
            active_connections: snapshot.active_connections,
        }
    }
}

/// Public metrics structure
#[derive(Debug, Clone)]
pub struct V2Metrics {
    pub udp_packets_received: u64,
    pub udp_packets_processed: u64,
    pub udp_packets_dropped: u64,
    pub packets_tunneled: u64,
    pub http_requests_received: u64,
    pub http_requests_processed: u64,
    pub http_requests_rate_limited: u64,
    pub game_slots_allocated: u64,
    pub active_connections: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_start_shutdown() {
        let mut config = TunnelV2Config::default();
        config.port = 0; // Use port 0 for automatic assignment
        let service = Arc::new(TunnelV2::with_config(config));
        let (shutdown_tx, handle) = service.clone().start().await.unwrap();

        sleep(Duration::from_millis(100)).await;
        shutdown_tx.send(()).unwrap();
        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn test_ping_limit_reached() {
        let config = TunnelV2Config::default();
        let service = TunnelV2::with_config(config);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        for _ in 0..DEFAULT_MAX_PINGS_PER_IP {
            assert!(!service.ping_limit_reached(ip));
        }
        assert!(service.ping_limit_reached(ip));
    }

    #[test]
    fn test_packet_validation() {
        let service = TunnelV2::with_config(TunnelV2Config::default());

        let valid_addr: SocketAddr = "192.168.1.1:1234".parse().unwrap();
        let loopback: SocketAddr = "127.0.0.1:1234".parse().unwrap();

        // Valid packets
        assert!(service.is_valid_packet(1, 2, &valid_addr));
        assert!(service.is_valid_packet(0, 0, &valid_addr));

        // Invalid packets
        assert!(!service.is_valid_packet(1, 1, &valid_addr));
        assert!(!service.is_valid_packet(1, 2, &loopback));
    }

    #[test]
    fn test_client_id_allocator() {
        let allocator = ClientIdAllocator::new();

        // Allocate some IDs
        let id1 = allocator.allocate().unwrap();
        let id2 = allocator.allocate().unwrap();

        assert_ne!(id1, 0);
        assert_ne!(id2, 0);
        assert_ne!(id1, id2);

        // Free and reallocate
        allocator.free(id1);
        let id3 = allocator.allocate().unwrap();
        assert_eq!(id3, id1); // Should reuse freed ID
    }

    #[test]
    fn test_allocate_client_ids() {
        let service = TunnelV2::with_config(TunnelV2Config::default());
        let mappings = DashMap::new();

        // Normal allocation
        let ids = service.allocate_client_ids(&mappings, 4);
        assert_eq!(ids.len(), 4);
        assert_eq!(mappings.len(), 4);

        // All IDs should be unique
        let mut unique_ids = std::collections::HashSet::new();
        for id in &ids {
            assert!(unique_ids.insert(*id));
        }
    }

    #[test]
    fn test_parse_clients_param() {
        assert_eq!(TunnelV2::parse_clients_param("clients=4"), 4);
        assert_eq!(TunnelV2::parse_clients_param("other=1&clients=3"), 3);
        assert_eq!(TunnelV2::parse_clients_param("clients=2&clients=5"), 5); // Last wins
        assert_eq!(TunnelV2::parse_clients_param("clients=abc"), 0);
        assert_eq!(TunnelV2::parse_clients_param(""), 0);
    }
}