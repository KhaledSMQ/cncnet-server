use crate::metrics::Metrics;
use crate::rate_limiter::RateLimiter;
use udp_relay_core::validate_address;

use socket2::{Domain, Protocol, Socket, Type};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::task::JoinSet;
use tokio::time::{self, Duration};
use tracing::{error, info};

const COUNTER_RESET_INTERVAL: Duration = Duration::from_secs(60);
const MAX_REQUESTS_PER_IP: usize = 20;
const STUN_ID: i16 = 26262;

pub struct PeerToPeer {
    port: u16,
    metrics: Arc<Metrics>,
    rate_limiter: Arc<RateLimiter>,
}

impl PeerToPeer {
    pub fn new(port: u16, metrics: Arc<Metrics>) -> Self {
        Self {
            port,
            metrics,
            rate_limiter: Arc::new(RateLimiter::new(60, MAX_REQUESTS_PER_IP as u32)),
        }
    }

    fn create_bound_socket(&self) -> Result<UdpSocket, Box<dyn std::error::Error>> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;

        #[cfg(unix)]
        socket.set_reuse_port(true)?;

        let _ = socket.set_recv_buffer_size(4 * 1024 * 1024);
        let _ = socket.set_send_buffer_size(4 * 1024 * 1024);

        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        socket.bind(&addr.into())?;
        socket.set_nonblocking(true)?;

        Ok(UdpSocket::from_std(socket.into())?)
    }

    pub async fn start(self) -> Result<(), Box<dyn std::error::Error>> {
        let this = Arc::new(self);

        info!("P2P service listening on port {}", this.port);

        let mut tasks = JoinSet::new();

        let rate_limiter = this.rate_limiter.clone();
        tasks.spawn(async move {
            let mut interval = time::interval(COUNTER_RESET_INTERVAL);
            loop {
                interval.tick().await;
                rate_limiter.reset();
            }
        });

        let num_workers = num_cpus::get().min(2).max(1);

        for worker_id in 0..num_workers {
            let worker_self = this.clone();
            let worker_socket = Arc::new(worker_self.create_bound_socket()?);

            tasks.spawn(async move {
                worker_self.receive_loop(worker_socket, worker_id).await;
            });
        }

        while let Some(res) = tasks.join_next().await {
            if let Err(e) = res {
                tracing::error!("P2P task panicked: {}", e);
            }
        }

        Ok(())
    }

    async fn receive_loop(&self, socket: Arc<UdpSocket>, worker_id: usize) {
        let mut buf = [0u8; 64];
        let mut send_buffer = [0u8; 40];
        {
            use rand::Rng;
            rand::rng().fill(&mut send_buffer[..]);
        }
        send_buffer[6] = ((STUN_ID >> 8) & 0xFF) as u8;
        send_buffer[7] = (STUN_ID & 0xFF) as u8;

        info!("P2P worker {} started on port {}", worker_id, self.port);

        loop {
            match socket.recv_from(&mut buf).await {
                Ok((size, addr)) => {
                    if size == 48 {
                        self.on_receive(&buf[..size], addr, &socket, &mut send_buffer)
                            .await;
                    }
                }
                Err(e) => {
                    error!("P2P UDP receive error on port {}: {}", self.port, e);
                    tokio::task::yield_now().await;
                }
            }
        }
    }

    async fn on_receive(
        &self,
        packet: &[u8],
        remote_addr: SocketAddr,
        socket: &UdpSocket,
        send_buffer: &mut [u8; 40],
    ) {
        if !validate_address(&remote_addr) {
            return;
        }

        if !self
            .rate_limiter
            .check_and_increment(&remote_addr.ip())
        {
            self.metrics
                .rate_limit_hits
                .fetch_add(1, Ordering::Relaxed);
            return;
        }

        self.metrics.p2p_requests.fetch_add(1, Ordering::Relaxed);

        let received_id = i16::from_be_bytes([packet[0], packet[1]]);
        if received_id != STUN_ID {
            return;
        }

        match remote_addr.ip() {
            std::net::IpAddr::V4(v4) => {
                let octets = v4.octets();
                send_buffer[0] = octets[0];
                send_buffer[1] = octets[1];
                send_buffer[2] = octets[2];
                send_buffer[3] = octets[3];
            }
            std::net::IpAddr::V6(_) => {
                send_buffer[0] = 0;
                send_buffer[1] = 0;
                send_buffer[2] = 0;
                send_buffer[3] = 0;
            }
        }

        let port_bytes = remote_addr.port().to_be_bytes();
        send_buffer[4] = port_bytes[0];
        send_buffer[5] = port_bytes[1];

        for i in 0..6 {
            send_buffer[i] ^= 0x20;
        }

        if let Err(e) = socket.send_to(send_buffer, remote_addr).await {
            error!("P2P send error: {}", e);
        } else {
            self.metrics.p2p_responses.fetch_add(1, Ordering::Relaxed);
        }
    }
}
