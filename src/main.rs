//! CnCNet Server - High-performance tunnel server for Command & Conquer games
//!
//! This server provides NAT traversal and tunneling capabilities for Red Alert 2
//! and Yuri's Revenge multiplayer games. Designed for low memory footprint and
//! high performance to replace the existing C# implementation.
//!
//! ## Architecture
//!
//! The server runs multiple services concurrently:
//! - Tunnel V3 (UDP): Main game tunneling protocol
//! - Tunnel V2 (UDP + HTTP): Legacy compatibility
//! - P2P STUN (UDP): NAT traversal for direct connections
//!
//! ## Performance Optimizations
//!
//! - Lock-free data structures for hot paths
//! - Pre-allocated memory pools
//! - Zero-copy packet forwarding
//! - Atomic operations for state management
//!
//! Author: Khaled Sameer <khaled.smq@hotmail.com>
//! Repository: https://github.com/khaledsmq/cncnet-server

use std::sync::Arc;
use anyhow::Result;
use tokio::signal;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use clap::Parser;

mod config;
mod net;
use config::Options;
use net::{peer_to_peer::PeerToPeerService, tunnel_v2::TunnelV2, tunnel_v3::TunnelV3};

/// Main entry point
///
/// Initializes logging, parses configuration, and starts all services
#[tokio::main]
async fn main() -> Result<()> {
    // Initialize structured logging with environment filter
    // Use RUST_LOG=debug for development, RUST_LOG=info for production
    init_logging();

    // Parse and validate command line options
    let mut options = Options::parse();
    // Validate options to ensure they are within acceptable ranges
    // This will also normalize values like port numbers and server name
    // to ensure they are safe and valid.
    options.validate();

    // Log startup information
    info!(
        name = %options.name,
        v3_port = options.port,
        v2_port = options.portv2,
        max_clients = options.maxclients,
        "CnCNet server starting"
    );

    // Display configuration summary
    print_configuration(&options);

    // Start all services and collect shutdown senders and handles
    let (shutdown_txs, handles) = start_services(options).await?;

    // Wait for shutdown signal
    wait_for_shutdown().await;

    info!("CnCNet server shutting down gracefully");

    // Send shutdown to all services
    for tx in shutdown_txs {
        match tx.send(()) {
            Err(_) => {
                warn!("Service shutdown channel already closed, service may not have started");
            }
            Ok(..) => {
                info!("Shutdown signal sent to service");
            }
        }
    }

    // Wait for all services to complete
    for handle in handles {
        match handle.await {
            Ok(result) => {
                if let Err(e) = result {
                    error!("Service error during shutdown: {}", e);
                }
            }
            Err(e) => error!("Service panicked during shutdown: {}", e),
        }
    }

    Ok(())
}

/// Initializes the logging system with optimal settings
///
/// Features:
/// - Structured logging with tracing
/// - Environment-based filtering (RUST_LOG)
/// - Efficient formatting for production
fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false) // Reduce log size
        .with_thread_ids(false) // Not needed for this server
        .with_file(false) // Reduce overhead in production
        .with_line_number(false)
        .compact() // Compact format for production
        .init();
}

/// Prints server configuration summary
fn print_configuration(options: &Options) {
    info!("=== CnCNet Server Configuration ===");
    info!("Server Name: {}", options.name);
    info!("Tunnel V3 Port: {}", options.port);
    info!("Tunnel V2 Port: {}", options.portv2);
    info!("Max Clients: {}", options.maxclients);
    info!("IP Limit V3: {}", options.iplimit);
    info!("IP Limit V2: {}", options.iplimitv2);
    info!(
        "Master Server: {}",
        if options.nomaster {
            "Disabled"
        } else {
            &options.master
        }
    );
    info!(
        "P2P Services: {}",
        if options.nop2p {
            "Disabled"
        } else {
            "Enabled (8054, 3478)"
        }
    );
    info!("==================================");
}

/// Starts all server services
///
/// Returns a vector of shutdown senders and a vector of join handles for graceful shutdown.
async fn start_services(
    options: Options,
) -> Result<(
    Vec<tokio::sync::oneshot::Sender<()>>,
    Vec<tokio::task::JoinHandle<Result<()>>>,
)> {
    let mut shutdown_txs = Vec::new();
    let mut handles = Vec::new();

    // Start P2P NAT traversal services if enabled
    if !options.nop2p {
        info!("Starting P2P NAT traversal services");

        // STUN on port 8054
        let service_8054 = PeerToPeerService::new(8054);
        let (tx, handle) = service_8054.start().await?;
        shutdown_txs.push(tx);
        handles.push(handle);

        // STUN on port 3478 (standard STUN port)
        let service_3478 = PeerToPeerService::new(3478);
        let (tx, handle) = service_3478.start().await?;
        shutdown_txs.push(tx);
        handles.push(handle);
    } else {
        info!("P2P NAT traversal disabled");
    }

    // Start Tunnel V3
    info!("Starting Tunnel V3 on port {}", options.port);
    let tunnel_v3 = TunnelV3::new(
        options.port,
        options.maxclients,
        options.name.clone(),
        options.nomaster,
        options.masterpw.clone(),
        options.maintpw.clone(),
        options.master.clone(),
        options.iplimit,
    );
    let (tx, handle) = tunnel_v3.start().await?;
    shutdown_txs.push(tx);
    handles.push(handle);

    // Start Tunnel V2
    info!("Starting Tunnel V2 on port {} (UDP + HTTP)", options.portv2);
    let tunnel_v2 = TunnelV2::new(
        options.portv2,
        options.maxclients,
        options.name,
        options.nomaster,
        options.masterpw,
        options.maintpw,
        options.master,
        options.iplimitv2,
    );
    let (tx, handle) = Arc::new(tunnel_v2).start().await?;
    shutdown_txs.push(tx);
    handles.push(handle);

    Ok((shutdown_txs, handles))
}

/// Waits for shutdown signal (Ctrl+C or SIGTERM)
///
/// Handles graceful shutdown on both Windows and Unix systems
async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm =
            signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");
        let mut sigint =
            signal(SignalKind::interrupt()).expect("Failed to register SIGINT handler");

        tokio::select! {
            _ = sigterm.recv() => {
                info!("Received SIGTERM");
            }
            _ = sigint.recv() => {
                info!("Received SIGINT (Ctrl+C)");
            }
            _ = signal::ctrl_c() => {
                info!("Received Ctrl+C");
            }
        }
    }

    #[cfg(not(unix))]
    {
        signal::ctrl_c()
            .await
            .expect("Failed to register Ctrl+C handler");
        info!("Received Ctrl+C");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_options_validation() {
        let mut opts = Options {
            port: 50001,
            portv2: 50000,
            name: "Test Server".to_string(),
            maxclients: 100,
            nomaster: true,
            masterpw: String::new(),
            maintpw: String::new(),
            master: String::new(),
            iplimit: 8,
            iplimitv2: 4,
            nop2p: false,
        };

        opts.validate();

        assert_eq!(opts.port, 50001);
        assert_eq!(opts.name, "Test Server");
        assert_eq!(opts.maxclients, 100);
    }
}
