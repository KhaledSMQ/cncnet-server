use crate::config::SharedConfig;
use crate::metrics::{LocalMetricsBatch, Metrics};
use crate::rate_limiter::RateLimiter;
use udp_relay_core::{create_dashmap_with_capacity, validate_address, TunnelClient};

use dashmap::DashMap;
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use socket2::{Domain, Protocol, Socket, Type};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time;
use tracing::{debug, error, info, warn};

const VERSION: u8 = 2;
const MASTER_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(60);
const MAX_PINGS_PER_IP: usize = 20;
const TIMEOUT_SECONDS: u64 = 30;
const MAX_HTTP_CONNECTIONS: usize = 100;
const MAX_PACKET_SIZE: usize = 1472;

#[derive(Clone)]
pub struct TunnelV2 {
    config: SharedConfig,
    metrics: Arc<Metrics>,
    mappings: Arc<DashMap<i16, Arc<TunnelClient>>>,
    ping_limiter: Arc<RateLimiter>,
    request_limiter: Arc<RateLimiter>,
    maintenance_mode: Arc<AtomicBool>,
    http_semaphore: Arc<Semaphore>,
    http_client: reqwest::Client,
    dropped_connections: Arc<AtomicU32>,
}

impl TunnelV2 {
    pub fn new(config: SharedConfig, metrics: Arc<Metrics>) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(1)
            .pool_idle_timeout(Duration::from_secs(30))
            .local_address("0.0.0.0".parse().ok())
            .build()
            .expect("Failed to create HTTP client");

        Self {
            ping_limiter: Arc::new(RateLimiter::new(60, MAX_PINGS_PER_IP as u32)),
            request_limiter: Arc::new(RateLimiter::new(60, config.ip_limit_v2 as u32)),
            mappings: Arc::new(create_dashmap_with_capacity(config.max_clients)),
            http_semaphore: Arc::new(Semaphore::new(MAX_HTTP_CONNECTIONS)),
            http_client,
            config,
            metrics,
            maintenance_mode: Arc::new(AtomicBool::new(false)),
            dropped_connections: Arc::new(AtomicU32::new(0)),
        }
    }

    fn create_bound_socket(&self) -> Result<UdpSocket, Box<dyn std::error::Error>> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;

        #[cfg(unix)]
        socket.set_reuse_port(true)?;

        let _ = socket.set_recv_buffer_size(8 * 1024 * 1024);
        let _ = socket.set_send_buffer_size(8 * 1024 * 1024);

        let addr = SocketAddr::from(([0, 0, 0, 0], self.config.tunnel_v2_port));
        socket.bind(&addr.into())?;
        socket.set_nonblocking(true)?;

        Ok(UdpSocket::from_std(socket.into())?)
    }

    pub async fn start(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error>> {
        info!("Tunnel V2 listening on port {}", self.config.tunnel_v2_port);

        let mut tasks = JoinSet::new();

        let http_self = self.clone();
        tasks.spawn(async move {
            if let Err(e) = http_self.start_http_server().await {
                error!("V2 HTTP server error: {}", e);
            }
        });

        let heartbeat_self = self.clone();
        tasks.spawn(async move {
            heartbeat_self.heartbeat_loop().await;
        });

        let num_receivers = num_cpus::get().min(4).max(1);

        info!("Starting {} V2 receive workers (per-socket)", num_receivers);

        for worker_id in 0..num_receivers {
            let self_clone = self.clone();
            let worker_socket = Arc::new(self_clone.create_bound_socket()?);

            tasks.spawn(async move {
                self_clone.receive_loop(worker_socket, worker_id).await;
            });
        }

        while let Some(res) = tasks.join_next().await {
            if let Err(e) = res {
                error!("V2 task panicked: {}", e);
            }
        }

        Ok(())
    }

    async fn receive_loop(&self, socket: Arc<UdpSocket>, worker_id: usize) {
        let mut buf = [0u8; MAX_PACKET_SIZE];
        let mut batch = LocalMetricsBatch::new();

        info!("V2 worker {} started", worker_id);

        loop {
            match socket.recv_from(&mut buf).await {
                Ok((size, addr)) => {
                    if size >= 4 && size <= MAX_PACKET_SIZE {
                        self.on_udp_receive(&buf[..size], addr, &socket, &mut batch).await;
                        batch.maybe_flush(&self.metrics);
                    }
                }
                Err(e) => {
                    batch.flush(&self.metrics);
                    if worker_id == 0 {
                        warn!("V2 UDP receive error: {}", e);
                    }
                    tokio::task::yield_now().await;
                }
            }
        }
    }

    async fn start_http_server(&self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = SocketAddr::from(([0, 0, 0, 0], self.config.tunnel_v2_port));
        let listener = TcpListener::bind(addr).await?;

        info!(
            "V2 HTTP server listening on port {}",
            self.config.tunnel_v2_port
        );

        loop {
            let (stream, remote_addr) = listener.accept().await?;

            let permit = match self.http_semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    drop(stream);
                    self.dropped_connections.fetch_add(1, Ordering::Relaxed);
                    self.metrics
                        .dropped_connections
                        .fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };

            let _ = stream.set_nodelay(true);
            let io = TokioIo::new(stream);
            let self_clone = self.clone();

            tokio::spawn(async move {
                let _permit = permit;
                let service = service_fn(move |req| {
                    let self_clone = self_clone.clone();
                    async move {
                        Ok::<_, Infallible>(
                            self_clone.handle_http_request(req, remote_addr).await,
                        )
                    }
                });

                if let Err(e) = http1::Builder::new()
                    .keep_alive(false)
                    .max_buf_size(8192)
                    .serve_connection(io, service)
                    .await
                {
                    debug!("V2 HTTP connection error: {}", e);
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

        if !self
            .request_limiter
            .check_and_increment(&remote_addr.ip())
        {
            self.metrics
                .rate_limit_hits
                .fetch_add(1, Ordering::Relaxed);
            return Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .header("Retry-After", "60")
                .body(Full::new(Bytes::from("Rate limit exceeded")))
                .unwrap();
        }

        match (req.method(), req.uri().path()) {
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
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::from("Invalid clients parameter (2-8)")))
                .unwrap();
        }

        if self.mappings.len() + clients > self.config.max_clients {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Full::new(Bytes::from("Not enough slots available")))
                .unwrap();
        }

        let mut client_ids = Vec::with_capacity(clients);
        use rand::Rng;
        let mut rng = rand::rng();
        let mut attempts = 0;
        const MAX_ATTEMPTS: usize = 1000;

        while client_ids.len() < clients && attempts < MAX_ATTEMPTS {
            attempts += 1;
            let client_id: i16 = rng.random();

            if client_id != 0 && !self.mappings.contains_key(&client_id) {
                let client = Arc::new(TunnelClient::new(TIMEOUT_SECONDS));
                self.mappings.insert(client_id, client);
                client_ids.push(client_id.to_string());
            }
        }

        if client_ids.len() < clients {
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
            info!("V2 maintenance mode toggled: {} -> {}", prev, !prev);

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

    async fn on_udp_receive(&self, packet: &[u8], remote_addr: SocketAddr, socket: &UdpSocket, batch: &mut LocalMetricsBatch) {
        if packet.len() < 4 {
            return;
        }

        let sender_id = i16::from_be_bytes([packet[0], packet[1]]);
        let receiver_id = i16::from_be_bytes([packet[2], packet[3]]);

        batch.record_v2_rx(packet.len() as u64);

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

    #[inline]
    fn validate_packet(&self, sender_id: i16, receiver_id: i16, addr: &SocketAddr) -> bool {
        if sender_id == receiver_id && sender_id != 0 {
            return false;
        }
        validate_address(addr)
    }

    async fn handle_ping(&self, packet: &[u8], remote_addr: SocketAddr, socket: &UdpSocket, batch: &mut LocalMetricsBatch) {
        self.metrics
            .v2_ping_requests
            .fetch_add(1, Ordering::Relaxed);

        let response = &packet[..12.min(packet.len())];
        if let Err(e) = socket.send_to(response, remote_addr).await {
            debug!("V2 ping send error: {}", e);
        } else {
            batch.record_v2_tx(response.len() as u64);
        }
    }

    async fn handle_data_forward(
        &self,
        sender_id: i16,
        receiver_id: i16,
        packet: &[u8],
        remote_addr: SocketAddr,
        socket: &UdpSocket,
        now: u64,
        batch: &mut LocalMetricsBatch,
    ) {
        if let Some(sender_entry) = self.mappings.get(&sender_id) {
            let sender = sender_entry.value().clone();
            drop(sender_entry);

            if sender.remote_ep.is_none() {
                let new_client =
                    Arc::new(TunnelClient::new_with_endpoint(remote_addr, TIMEOUT_SECONDS));
                self.mappings.insert(sender_id, new_client);
                debug!("New V2 connection from {} ID {}", remote_addr, sender_id);
            } else if let Some(ep) = sender.remote_ep {
                if ep != remote_addr {
                    if sender.is_timed_out() {
                        let new_client = Arc::new(TunnelClient::new_with_endpoint(
                            remote_addr,
                            TIMEOUT_SECONDS,
                        ));
                        self.mappings.insert(sender_id, new_client);
                    } else {
                        return;
                    }
                } else {
                    sender.set_last_receive_tick_at(now);
                    sender.update_stats(packet.len(), 0, now);
                }
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
                    debug!("V2 relay error: {}", e);
                } else {
                    batch.record_v2_tx(packet.len() as u64);
                    receiver_client.update_stats(0, packet.len(), now);
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

            let dropped = self.dropped_connections.swap(0, Ordering::AcqRel);
            if dropped > 0 {
                warn!("Dropped {} V2 HTTP connections due to overload", dropped);
            }
        }
    }

    async fn cleanup_expired_mappings(&self) {
        self.mappings.retain(|_key, value| !value.is_timed_out());

        self.metrics
            .v2_active_clients
            .store(self.mappings.len(), Ordering::Relaxed);
        self.ping_limiter.reset();

        info!("V2 cleanup: {} active clients", self.mappings.len());
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

        match self.http_client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                debug!("V2 master announce sent");
            }
            Ok(resp) => {
                warn!("V2 master announce failed: {}", resp.status());
            }
            Err(e) => {
                warn!("V2 master announce error: {}", e);
            }
        }
    }
}
