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

/// Service startup result tracking
#[derive(Debug)]
struct ServiceStartupResult {
    /// Service name
    name: &'static str,
    /// Whether the service is critical (server cannot run without it)
    critical: bool,
    /// Whether the service started successfully
    success: bool,
    /// Shutdown sender if started successfully
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Task handle if started successfully
    handle: Option<tokio::task::JoinHandle<Result<()>>>,
}

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

    // Check system socket buffer limits
    net::utils::log_socket_limits();

    // Start all services and collect shutdown senders and handles
    let services = start_services(options).await?;

    // Extract running services
    let mut shutdown_txs: Vec<tokio::sync::oneshot::Sender<()>> = Vec::new();
    let mut handles: Vec<tokio::task::JoinHandle<Result<()>>> = Vec::new();

    for service in services {
        if service.success {
            if let (Some(tx), Some(handle)) = (service.shutdown_tx, service.handle) {
                shutdown_txs.push(tx);
                handles.push(handle);
            }
        }
    }

    // Wait for shutdown signal
    wait_for_shutdown().await;

    info!("CnCNet server shutting down gracefully");

    // Send shutdown to all services
    for tx in shutdown_txs.drain(..) {
        match tx.send(()) {
            Err(_) => {
                warn!("Service shutdown channel already closed");
            }
            Ok(()) => {
                info!("Shutdown signal sent to service");
            }
        }
    }

    // Wait for all services to complete with timeout
    let shutdown_timeout = tokio::time::Duration::from_secs(10);
    let shutdown_future = async {
        for handle in handles.drain(..) {
            match handle.await {
                Ok(result) => {
                    if let Err(e) = result {
                        error!("Service error during shutdown: {}", e);
                    }
                }
                Err(e) => error!("Service panicked during shutdown: {}", e),
            }
        }
    };

    match tokio::time::timeout(shutdown_timeout, shutdown_future).await {
        Ok(_) => info!("All services shut down successfully"),
        Err(_) => error!("Some services did not shut down within timeout"),
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
/// Returns a vector of service startup results for analysis
async fn start_services(options: Options) -> Result<Vec<ServiceStartupResult>> {
    let mut services = Vec::new();

    // Start P2P NAT traversal services if enabled (OPTIONAL)
    if !options.nop2p {
        info!("Starting P2P NAT traversal services");

        // STUN on port 8054
        let result = match PeerToPeerService::new(8054).start().await {
            Ok((tx, handle)) => {
                info!("P2P service started on port 8054");
                ServiceStartupResult {
                    name: "P2P 8054",
                    critical: false,
                    success: true,
                    shutdown_tx: Some(tx),
                    handle: Some(handle),
                }
            }
            Err(e) => {
                error!("Failed to start P2P service on port 8054: {}", e);
                ServiceStartupResult {
                    name: "P2P 8054",
                    critical: false,
                    success: false,
                    shutdown_tx: None,
                    handle: None,
                }
            }
        };
        services.push(result);

        // STUN on port 3478 (standard STUN port)
        let result = match PeerToPeerService::new(3478).start().await {
            Ok((tx, handle)) => {
                info!("P2P service started on port 3478");
                ServiceStartupResult {
                    name: "P2P 3478",
                    critical: false,
                    success: true,
                    shutdown_tx: Some(tx),
                    handle: Some(handle),
                }
            }
            Err(e) => {
                error!("Failed to start P2P service on port 3478: {}", e);
                ServiceStartupResult {
                    name: "P2P 3478",
                    critical: false,
                    success: false,
                    shutdown_tx: None,
                    handle: None,
                }
            }
        };
        services.push(result);
    } else {
        info!("P2P NAT traversal disabled");
    }

    // Start Tunnel V3 (CRITICAL)
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

    let result = match tunnel_v3.start().await {
        Ok((tx, handle)) => {
            info!("Tunnel V3 started successfully");
            ServiceStartupResult {
                name: "Tunnel V3",
                critical: true,
                success: true,
                shutdown_tx: Some(tx),
                handle: Some(handle),
            }
        }
        Err(e) => {
            error!("Failed to start Tunnel V3: {}", e);
            ServiceStartupResult {
                name: "Tunnel V3",
                critical: true,
                success: false,
                shutdown_tx: None,
                handle: None,
            }
        }
    };
    services.push(result);

    // Start Tunnel V2 (CRITICAL)
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

    let result = match Arc::new(tunnel_v2).start().await {
        Ok((tx, handle)) => {
            info!("Tunnel V2 started successfully");
            ServiceStartupResult {
                name: "Tunnel V2",
                critical: true,
                success: true,
                shutdown_tx: Some(tx),
                handle: Some(handle),
            }
        }
        Err(e) => {
            error!("Failed to start Tunnel V2: {}", e);
            ServiceStartupResult {
                name: "Tunnel V2",
                critical: true,
                success: false,
                shutdown_tx: None,
                handle: None,
            }
        }
    };
    services.push(result);

    // Check if any critical services failed
    let critical_failures: Vec<&str> = services
        .iter()
        .filter(|s| s.critical && !s.success)
        .map(|s| s.name)
        .collect();

    if !critical_failures.is_empty() {
        error!("Critical services failed to start: {:?}", critical_failures);

        // Log optional service status
        let optional_status: Vec<(&str, bool)> = services
            .iter()
            .filter(|s| !s.critical)
            .map(|s| (s.name, s.success))
            .collect();

        if !optional_status.is_empty() {
            info!("Optional service status: {:?}", optional_status);
        }

        // Shut down all started services
        error!("Shutting down all started services due to critical failures");
        for mut service in services.drain(..) {
            if let (Some(tx), Some(handle)) = (service.shutdown_tx.take(), service.handle.take()) {
                let _ = tx.send(());
                let _ = tokio::time::timeout(
                    tokio::time::Duration::from_secs(2),
                    handle,
                ).await;
            }
        }

        anyhow::bail!("Failed to start critical services: {:?}", critical_failures);
    }

    // Log service summary
    let running_services: Vec<&str> = services
        .iter()
        .filter(|s| s.success)
        .map(|s| s.name)
        .collect();

    info!("Successfully started services: {:?}", running_services);

    Ok(services)
}

/// Waits for shutdown signal (Ctrl+C or SIGTERM)
///
/// Handles graceful shutdown on both Windows and Unix systems
async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(sig) => sig,
            Err(e) => {
                error!("Failed to register SIGTERM handler: {}", e);
                // Fall back to just Ctrl+C
                signal::ctrl_c()
                    .await
                    .expect("Failed to register Ctrl+C handler");
                info!("Received Ctrl+C");
                return;
            }
        };

        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(sig) => sig,
            Err(e) => {
                error!("Failed to register SIGINT handler: {}", e);
                // Fall back to just Ctrl+C
                signal::ctrl_c()
                    .await
                    .expect("Failed to register Ctrl+C handler");
                info!("Received Ctrl+C");
                return;
            }
        };

        // Also handle SIGHUP for completeness
        // let mut sighup = match signal(SignalKind::hangup()) {
        //     Ok(sig) => sig,
        //     Err(e) => {
        //         warn!("Failed to register SIGHUP handler: {}", e);
        //         // SIGHUP is optional, continue without it
        //         // Create a never-completing future as placeholder
        //         let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
        //         std::mem::forget(tx); // Ensure it never sends
        //         rx
        //     }
        // };
        let mut sighup = match signal(SignalKind::hangup()) {
            Ok(sig) => sig,
            Err(e) => {
                warn!("Failed to register SIGHUP handler: {}", e);
                // SIGHUP is optional, continue without it
                return;
            }
        };


        tokio::select! {
            _ = sigterm.recv() => {
                info!("Received SIGTERM");
            }
            _ = sigint.recv() => {
                info!("Received SIGINT (Ctrl+C)");
            }
            _ = sighup.recv() => {
                info!("Received SIGHUP");
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

    #[test]
    fn test_service_startup_result() {
        let result = ServiceStartupResult {
            name: "Test Service",
            critical: true,
            success: false,
            shutdown_tx: None,
            handle: None,
        };

        assert_eq!(result.name, "Test Service");
        assert!(result.critical);
        assert!(!result.success);
    }
}