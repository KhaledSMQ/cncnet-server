//! CnCNet Server Library
//!
//! This library provides the core functionality for the CnCNet tunnel server.

pub mod config;
pub mod net;

// Re-export commonly used types for tests
pub use config::Options;
pub use net::{
    NetworkError,
    peer_to_peer::PeerToPeerService,
    tunnel_v2::TunnelV2,
    tunnel_v3::TunnelV3,
};