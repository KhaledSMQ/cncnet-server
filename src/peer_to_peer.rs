use crate::metrics::Metrics;
use crate::rate_limiter::RateLimiter;
use bytes::{BufMut, BytesMut};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::time::{self, Duration};
use tracing::{debug, error, info};

const COUNTER_RESET_INTERVAL: Duration = Duration::from_secs(60);
const MAX_REQUESTS_PER_IP: usize = 20;
const MAX_CONNECTIONS_GLOBAL: usize = 5000;
const STUN_ID: i16 = 26262;

pub struct PeerToPeer {
    port: u16,
    metrics: Arc<Metrics>,
    rate_limiter: Arc<RateLimiter>,
    send_buffer: Vec<u8>,
}

impl PeerToPeer {
    pub fn new(port: u16, metrics: Arc<Metrics>) -> Self {
        // Initialize send buffer with random data
        let mut send_buffer = vec![0u8; 40];
        use rand::Rng;
        rand::thread_rng().fill(&mut send_buffer[..]);

        // Set STUN ID at position 6-7
        send_buffer[6] = ((STUN_ID >> 8) & 0xFF) as u8;
        send_buffer[7] = (STUN_ID & 0xFF) as u8;

        Self {
            port,
            metrics,
            rate_limiter: Arc::new(RateLimiter::new(
                60,
                MAX_REQUESTS_PER_IP,
                MAX_CONNECTIONS_GLOBAL,
            )),
            send_buffer,
        }
    }

    pub async fn start(self) -> Result<(), Box<dyn std::error::Error>> {
        let socket = Arc::new(UdpSocket::bind(("0.0.0.0", self.port)).await?);

        info!("P2P service listening on port {}", self.port);

        // Spawn rate limiter reset task
        let rate_limiter = self.rate_limiter.clone();
        tokio::spawn(async move {
            let mut interval = time::interval(COUNTER_RESET_INTERVAL);
            loop {
                interval.tick().await;
                rate_limiter.reset();
            }
        });

        // Main receive loop
        let mut buf = BytesMut::with_capacity(64);
        buf.resize(64, 0);

        loop {
            match socket.recv_from(&mut buf).await {
                Ok((size, addr)) => {
                    if size == 48 {
                        let packet = buf[..size].to_vec();
                        let socket_clone = socket.clone();
                        let metrics_clone = self.metrics.clone();
                        let rate_limiter_clone = self.rate_limiter.clone();
                        let mut send_buffer = self.send_buffer.clone();

                        tokio::spawn(async move {
                            Self::on_receive(
                                packet,
                                addr,
                                socket_clone,
                                metrics_clone,
                                rate_limiter_clone,
                                &mut send_buffer,
                            ).await;
                        });
                    }
                }
                Err(e) => {
                    error!("P2P UDP receive error on port {}: {}", self.port, e);
                }
            }
        }
    }

    async fn on_receive(
        packet: Vec<u8>,
        remote_addr: SocketAddr,
        socket: Arc<UdpSocket>,
        metrics: Arc<Metrics>,
        rate_limiter: Arc<RateLimiter>,
        send_buffer: &mut [u8],
    ) {
        debug!(
            "[{} UTC] PeerToPeerUtil: Received packet from {} with size {}",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
            remote_addr,
            packet.len()
        );

        // Validate source
        if remote_addr.ip() == IpAddr::V4(Ipv4Addr::LOCALHOST)
            || remote_addr.ip() == IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            || remote_addr.ip() == IpAddr::V4(Ipv4Addr::BROADCAST)
            || remote_addr.port() == 0
        {
            debug!(
                "PeerToPeerUtil: Ignoring packet from {} due to invalid address or port",
                remote_addr
            );
            return;
        }

        // Check rate limit
        if !rate_limiter.check_and_increment(&remote_addr.ip()) {
            debug!(
                "PeerToPeerUtil: Connection limit reached for {}. Ignoring packet.",
                remote_addr.ip()
            );
            metrics.rate_limit_hits.fetch_add(1, Ordering::Relaxed);
            return;
        }

        metrics.p2p_requests.fetch_add(1, Ordering::Relaxed);

        // Check STUN ID
        let received_id = i16::from_be_bytes([packet[0], packet[1]]);
        if received_id == STUN_ID {
            // Prepare response with client's address and port
            match remote_addr.ip() {
                IpAddr::V4(v4) => {
                    let octets = v4.octets();
                    send_buffer[0] = octets[0];
                    send_buffer[1] = octets[1];
                    send_buffer[2] = octets[2];
                    send_buffer[3] = octets[3];
                }
                IpAddr::V6(_) => {
                    // For IPv6, we'd need a different approach
                    // For now, just use zeros
                    send_buffer[0] = 0;
                    send_buffer[1] = 0;
                    send_buffer[2] = 0;
                    send_buffer[3] = 0;
                }
            }

            // Set port (big-endian)
            let port_bytes = remote_addr.port().to_be_bytes();
            send_buffer[4] = port_bytes[0];
            send_buffer[5] = port_bytes[1];

            // Obfuscate the first 6 bytes
            for i in 0..6 {
                send_buffer[i] ^= 0x20;
            }

            debug!(
                "PeerToPeerUtil: Sending response to {} with port {}",
                remote_addr.ip(),
                remote_addr.port()
            );

            if let Err(e) = socket.send_to(send_buffer, remote_addr).await {
                error!("Failed to send P2P response: {}", e);
            } else {
                metrics.p2p_responses.fetch_add(1, Ordering::Relaxed);
            }
        } else {
            debug!(
                "PeerToPeerUtil: Ignoring packet from {} due to invalid STUN ID",
                remote_addr
            );
        }
    }
}

// Add chrono and rand dependencies
use chrono;
use rand;