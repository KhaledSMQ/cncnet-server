use crate::buffer_pool;
use crate::config::SharedConfig;
use crate::metrics::Metrics;
use crate::rate_limiter::{ConnectionLimiter, RateLimiter};
use crate::tunnel_client::TunnelClient;
use bytes::{Buf, BytesMut};
use dashmap::DashMap;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rand;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::Semaphore;
use tokio::time;
use tracing::{debug, error, info, warn};

const VERSION: u8 = 2;
const MASTER_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(60);
const MAX_PINGS_PER_IP: usize = 20;
const MAX_PINGS_GLOBAL: usize = 5000;
const MAX_REQUESTS_GLOBAL: usize = 1000;
const TIMEOUT_SECONDS: u64 = 30;
const MAX_HTTP_CONNECTIONS: usize = 100;
const MAX_PACKET_SIZE: usize = 1472;

#[derive(Clone)]
pub struct TunnelV2 {
    config: SharedConfig,
    metrics: Arc<Metrics>,
    mappings: Arc<DashMap<i16, Arc<TunnelClient>>>,
    connection_limiter: Arc<ConnectionLimiter>,
    ping_limiter: Arc<RateLimiter>,
    request_limiter: Arc<RateLimiter>,
    maintenance_mode: Arc<AtomicBool>,
    task_limiter: Arc<Semaphore>,
    http_semaphore: Arc<Semaphore>,
    dropped_connections: Arc<AtomicU32>,
}

impl TunnelV2 {
    pub fn new(config: SharedConfig, metrics: Arc<Metrics>, task_limiter: Arc<Semaphore>) -> Self {
        Self {
            connection_limiter: Arc::new(ConnectionLimiter::new(config.ip_limit_v2)),
            ping_limiter: Arc::new(RateLimiter::new(60, MAX_PINGS_PER_IP, MAX_PINGS_GLOBAL)),
            request_limiter: Arc::new(RateLimiter::new(
                60,
                config.ip_limit_v2,
                MAX_REQUESTS_GLOBAL,
            )),
            mappings: Arc::new(DashMap::with_capacity(config.max_clients)),
            http_semaphore: Arc::new(Semaphore::new(MAX_HTTP_CONNECTIONS)),
            config,
            metrics,
            maintenance_mode: Arc::new(AtomicBool::new(false)),
            task_limiter,
            dropped_connections: Arc::new(AtomicU32::new(0)),
        }
    }

    pub async fn start(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error>> {
        // Start HTTP server
        let http_self = self.clone();
        tokio::spawn(async move {
            if let Err(e) = http_self.start_http_server().await {
                error!("HTTP server error: {}", e);
            }
        });

        // Create optimized UDP socket
        let socket = self.create_optimized_socket().await?;
        let udp_socket = Arc::new(socket);

        info!("Tunnel V2 listening on port {}", self.config.tunnel_v2_port);

        // Spawn heartbeat task
        let heartbeat_self = self.clone();
        tokio::spawn(async move {
            heartbeat_self.heartbeat_loop().await;
        });

        // Use multiple receive workers for better performance
        let num_receivers = num_cpus::get().min(4).max(1);
        let mut receivers = Vec::with_capacity(num_receivers);

        for worker_id in 0..num_receivers {
            let self_clone = self.clone();
            let socket_clone = udp_socket.clone();

            receivers.push(tokio::spawn(async move {
                self_clone.receive_loop(socket_clone, worker_id).await;
            }));
        }

        // Wait for all receivers
        for receiver in receivers {
            let _ = receiver.await;
        }

        Ok(())
    }

    async fn create_optimized_socket(&self) -> Result<UdpSocket, Box<dyn std::error::Error>> {
        use socket2::{Domain, Protocol, Socket, Type};

        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

        // Set socket options
        socket.set_reuse_address(true)?;

        #[cfg(unix)]
        socket.set_reuse_port(true)?;

        // Increase buffer sizes
        let _ = socket.set_recv_buffer_size(8 * 1024 * 1024);
        let _ = socket.set_send_buffer_size(8 * 1024 * 1024);

        // Bind and convert
        let addr = SocketAddr::from(([0, 0, 0, 0], self.config.tunnel_v2_port));
        socket.bind(&addr.into())?;
        socket.set_nonblocking(true)?;

        Ok(UdpSocket::from_std(socket.into())?)
    }

    async fn receive_loop(&self, socket: Arc<UdpSocket>, worker_id: usize) {
        let pool = buffer_pool::get_pool();
        let mut buf = pool.acquire_medium();

        info!("Tunnel V2 worker {} started", worker_id);

        loop {
            buf.resize(MAX_PACKET_SIZE, 0);

            match socket.recv_from(&mut buf).await {
                Ok((size, addr)) => {
                    if size >= 4 && size <= MAX_PACKET_SIZE {
                        let packet = buf[..size].to_vec();
                        let self_clone = self.clone();
                        let socket_clone = socket.clone();

                        // Try to acquire permit
                        let permit = match self.task_limiter.clone().try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                self.metrics.dropped_packets.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                        };

                        tokio::spawn(async move {
                            let _permit = permit;
                            self_clone.on_udp_receive(packet, addr, socket_clone).await;
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(Duration::from_micros(10)).await;
                }
                Err(e) => {
                    if worker_id == 0 {
                        warn!("UDP receive error: {}", e);
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        }
    }

    async fn start_http_server(&self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = SocketAddr::from(([0, 0, 0, 0], self.config.tunnel_v2_port));
        let listener = TcpListener::bind(addr).await?;

        info!(
            "Tunnel V2 HTTP server listening on port {}",
            self.config.tunnel_v2_port
        );

        loop {
            let (stream, remote_addr) = listener.accept().await?;

            // Apply connection limiting
            let permit = match self.http_semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    // Too many connections, close immediately
                    drop(stream);
                    self.dropped_connections.fetch_add(1, Ordering::Relaxed);
                    self.metrics
                        .dropped_connections
                        .fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };

            // Set TCP nodelay for lower latency
            let _ = stream.set_nodelay(true);

            let io = TokioIo::new(stream);
            let self_clone = self.clone();

            tokio::spawn(async move {
                let _permit = permit; // Hold permit until connection closes

                let service = service_fn(move |req| {
                    let self_clone = self_clone.clone();
                    let remote_addr = remote_addr;
                    async move {
                        Ok::<_, Infallible>(self_clone.handle_http_request(req, remote_addr).await)
                    }
                });

                // Disable keep-alive to free connections faster
                if let Err(e) = http1::Builder::new()
                    .keep_alive(false)
                    .max_buf_size(8192)
                    .serve_connection(io, service)
                    .await
                {
                    debug!("HTTP connection error: {}", e);
                }
            });
        }
    }

    async fn handle_http_request(
        &self,
        req: Request<hyper::body::Incoming>,
        remote_addr: SocketAddr,
    ) -> Response<Full<Bytes>> {
        self.metrics
            .v2_http_requests
            .fetch_add(1, Ordering::Relaxed);

        // Apply rate limiting
        if !self.request_limiter.check_and_increment(&remote_addr.ip()) {
            self.metrics.rate_limit_hits.fetch_add(1, Ordering::Relaxed);
            return Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .header("Retry-After", "60")
                .body(Full::new(Bytes::from("Rate limit exceeded")))
                .unwrap();
        }

        let path = req.uri().path();

        match (req.method(), path) {
            (&Method::GET, "/status") => self.handle_status().await,
            (&Method::GET, "/request") => self.handle_request(req.uri().query()).await,
            (&Method::GET, path) if path.starts_with("/maintenance/") => {
                self.handle_maintenance(path).await
            }
            (&Method::GET, "/health") => Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::from("OK")))
                .unwrap(),
            _ => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::from("Not Found")))
                .unwrap(),
        }
    }

    async fn handle_status(&self) -> Response<Full<Bytes>> {
        let active = self.mappings.len();
        let free = self.config.max_clients.saturating_sub(active);
        let dropped = self.dropped_connections.load(Ordering::Relaxed);

        let status = format!(
            "{} slots free.\n{} slots in use.\n{} connections dropped.\n",
            free, active, dropped
        );

        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/plain")
            .body(Full::new(Bytes::from(status)))
            .unwrap()
    }

    async fn handle_request(&self, query: Option<&str>) -> Response<Full<Bytes>> {
        if self.maintenance_mode.load(Ordering::Acquire) {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Full::new(Bytes::from("Server in maintenance mode")))
                .unwrap();
        }

        let clients = query
            .and_then(|q| {
                q.split('&')
                    .find(|p| p.starts_with("clients="))
                    .and_then(|p| p.strip_prefix("clients="))
                    .and_then(|v| v.parse::<usize>().ok())
            })
            .unwrap_or(0);

        if clients < 2 || clients > 8 {
            debug!("Invalid clients parameter: {}", clients);
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::from("Invalid clients parameter (2-8)")))
                .unwrap();
        }

        // Check capacity
        if self.mappings.len() + clients > self.config.max_clients {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Full::new(Bytes::from("Not enough slots available")))
                .unwrap();
        }

        let mut client_ids = Vec::with_capacity(clients);
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let mut attempts = 0;
        const MAX_ATTEMPTS: usize = 1000;

        while client_ids.len() < clients && attempts < MAX_ATTEMPTS {
            attempts += 1;
            let client_id = rng.gen::<i16>();

            if client_id != 0 && !self.mappings.contains_key(&client_id) {
                let client = Arc::new(TunnelClient::new(TIMEOUT_SECONDS));
                self.mappings.insert(client_id, client);
                client_ids.push(client_id.to_string());
            }
        }

        if client_ids.len() < clients {
            // Rollback if we couldn't allocate all requested IDs
            for id_str in &client_ids {
                if let Ok(id) = id_str.parse::<i16>() {
                    self.mappings.remove(&id);
                }
            }

            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Full::new(Bytes::from(
                    "Could not allocate requested clients",
                )))
                .unwrap();
        }

        self.metrics
            .v2_active_clients
            .store(self.mappings.len(), Ordering::Relaxed);

        let response = format!("[{}]", client_ids.join(","));
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(response)))
            .unwrap()
    }

    async fn handle_maintenance(&self, path: &str) -> Response<Full<Bytes>> {
        if self.config.maintenance_password.is_empty() {
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(Full::new(Bytes::from(
                    "Maintenance password not configured",
                )))
                .unwrap();
        }

        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() >= 3 && parts[2] == self.config.maintenance_password {
            let prev = self.maintenance_mode.fetch_xor(true, Ordering::AcqRel);
            info!("Maintenance mode toggled via HTTP: {} -> {}", prev, !prev);

            let status = if !prev { "enabled" } else { "disabled" };
            Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::from(format!(
                    "Maintenance mode {}",
                    status
                ))))
                .unwrap()
        } else {
            Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(Full::new(Bytes::from("Invalid maintenance password")))
                .unwrap()
        }
    }

    async fn on_udp_receive(
        &self,
        packet: Vec<u8>,
        remote_addr: SocketAddr,
        socket: Arc<UdpSocket>,
    ) {
        if packet.len() < 4 {
            return;
        }

        let sender_id = i16::from_be_bytes([packet[0], packet[1]]);
        let receiver_id = i16::from_be_bytes([packet[2], packet[3]]);

        self.metrics
            .v2_packets_received
            .fetch_add(1, Ordering::Relaxed);
        self.metrics
            .v2_bytes_received
            .fetch_add(packet.len() as u64, Ordering::Relaxed);

        // Validate packet
        if !self.validate_packet(sender_id, receiver_id, &remote_addr) {
            debug!("Ignoring invalid packet from {}", remote_addr);
            return;
        }

        // Handle ping
        if sender_id == 0 && receiver_id == 0 {
            if packet.len() == 50 && self.ping_limiter.check_and_increment(&remote_addr.ip()) {
                self.handle_ping(&packet, remote_addr, socket).await;
            }
            return;
        }

        // Check maintenance mode
        if self.maintenance_mode.load(Ordering::Acquire) {
            return;
        }

        // Handle data forwarding
        self.handle_data_forward(sender_id, receiver_id, packet, remote_addr, socket)
            .await;
    }

    #[inline]
    fn validate_packet(&self, sender_id: i16, receiver_id: i16, addr: &SocketAddr) -> bool {
        // Check for invalid combinations
        if sender_id == receiver_id && sender_id != 0 {
            return false;
        }

        // Check for invalid addresses
        let ip_valid = match addr.ip() {
            IpAddr::V4(v4) => {
                !v4.is_loopback()
                    && !v4.is_unspecified()
                    && !v4.is_broadcast()
                    && !v4.is_multicast()
            }
            IpAddr::V6(v6) => !v6.is_loopback() && !v6.is_unspecified() && !v6.is_multicast(),
        };

        ip_valid && addr.port() != 0
    }

    async fn handle_ping(&self, packet: &[u8], remote_addr: SocketAddr, socket: Arc<UdpSocket>) {
        self.metrics
            .v2_ping_requests
            .fetch_add(1, Ordering::Relaxed);
        debug!("Ping from {}", remote_addr);

        let response = &packet[..12.min(packet.len())];
        if let Err(e) = socket.send_to(response, remote_addr).await {
            debug!("Failed to send ping response: {}", e);
        } else {
            self.metrics.v2_packets_sent.fetch_add(1, Ordering::Relaxed);
            self.metrics
                .v2_bytes_sent
                .fetch_add(response.len() as u64, Ordering::Relaxed);
        }
    }

    async fn handle_data_forward(
        &self,
        sender_id: i16,
        receiver_id: i16,
        packet: Vec<u8>,
        remote_addr: SocketAddr,
        socket: Arc<UdpSocket>,
    ) {
        // Handle sender
        if let Some(sender_entry) = self.mappings.get(&sender_id) {
            let sender = sender_entry.clone();

            if sender.remote_ep.is_none() {
                // First packet from this client
                drop(sender_entry);
                let new_client = Arc::new(TunnelClient::new_with_endpoint(
                    remote_addr,
                    TIMEOUT_SECONDS,
                ));
                self.mappings.insert(sender_id, new_client.clone());
                debug!(
                    "New V2 connection from {} with ID {}",
                    remote_addr, sender_id
                );
            } else if let Some(ep) = sender.remote_ep {
                if ep != remote_addr {
                    // IP changed - security check
                    if sender.is_timed_out() {
                        // Allow IP change for timed out connections
                        drop(sender_entry);
                        let new_client = Arc::new(TunnelClient::new_with_endpoint(
                            remote_addr,
                            TIMEOUT_SECONDS,
                        ));
                        self.mappings.insert(sender_id, new_client);
                    } else {
                        debug!(
                            "Rejecting IP change for active connection {} from {} to {}",
                            sender_id, ep, remote_addr
                        );
                        return;
                    }
                } else {
                    sender.set_last_receive_tick();
                    sender.update_stats(packet.len(), 0);
                }
            }

            // Forward to receiver
            if let Some(receiver) = self.mappings.get(&receiver_id) {
                if let Some(receiver_ep) = receiver.remote_ep {
                    if receiver_ep != remote_addr {
                        debug!("Relaying packet from {} to {}", remote_addr, receiver_ep);

                        if let Err(e) = socket.send_to(&packet, receiver_ep).await {
                            debug!("Failed to relay packet: {}", e);
                        } else {
                            self.metrics.v2_packets_sent.fetch_add(1, Ordering::Relaxed);
                            self.metrics
                                .v2_bytes_sent
                                .fetch_add(packet.len() as u64, Ordering::Relaxed);
                            receiver.update_stats(0, packet.len());
                        }
                    }
                } else {
                    debug!("No receiver endpoint for ID {}", receiver_id);
                }
            }
        }
    }

    async fn heartbeat_loop(&self) {
        let mut interval = time::interval(MASTER_ANNOUNCE_INTERVAL);

        loop {
            interval.tick().await;

            self.cleanup_expired_mappings().await;
            self.request_limiter.reset();

            if !self.config.no_master_announce {
                self.send_master_announce().await;
            }

            // Log stats periodically
            let dropped = self.dropped_connections.swap(0, Ordering::AcqRel);
            if dropped > 0 {
                warn!("Dropped {} HTTP connections due to overload", dropped);
            }
        }
    }

    async fn cleanup_expired_mappings(&self) {
        let mut expired = Vec::new();

        for entry in self.mappings.iter() {
            if entry.value().is_timed_out() {
                expired.push(*entry.key());
            }
        }

        for id in expired {
            self.mappings.remove(&id);
            debug!("Removed expired V2 client {}", id);
        }

        self.metrics
            .v2_active_clients
            .store(self.mappings.len(), Ordering::Relaxed);
        self.ping_limiter.reset();

        info!(
            "V2 cleanup complete: {} active clients",
            self.mappings.len()
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
            self.config.tunnel_v2_port,
            clients,
            self.config.max_clients,
            urlencoding::encode(&self.config.master_password),
            maintenance
        );

        match reqwest::get(&url).await {
            Ok(resp) if resp.status().is_success() => {
                debug!("V2 master announce sent successfully");
            }
            Ok(resp) => {
                warn!("V2 master announce failed with status: {}", resp.status());
            }
            Err(e) => {
                warn!("Failed to send V2 master announce: {}", e);
            }
        }
    }
}
