//! Network module containing all networking components
//!
//! This module includes tunnel implementations and peer-to-peer services

pub mod peer_to_peer;
pub mod tunnel_client;
pub mod tunnel_v2;
pub mod tunnel_v3;
pub mod rate_limiter;
pub mod utils;

/// Common constants used across network modules
pub mod constants {
    /// Master server announce interval in milliseconds
    pub const MASTER_ANNOUNCE_INTERVAL: u64 = 60_000;

    /// Command rate limit - 1 command per X seconds
    pub const COMMAND_RATE_LIMIT: u64 = 60;

    /// Client timeout in seconds
    pub const CLIENT_TIMEOUT: u64 = 30;

    /// STUN identifier for peer-to-peer
    pub const STUN_ID: i16 = 26262;
}

/// Common error types for network operations
#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid packet format")]
    InvalidPacket,

    #[error("Client limit reached")]
    ClientLimitReached,

    #[error("IP limit reached")]
    IpLimitReached,

    #[error("Maintenance mode enabled")]
    MaintenanceMode,

    #[error("Unauthorized")]
    Unauthorized,
}