mod config;
mod errors;
mod metrics;
mod tunnel_v2;
mod tunnel_v3;
mod peer_to_peer;
mod tunnel_client;
mod rate_limiter;
mod buffer_pool;
mod shutdown;
mod health;

use anyhow::Result;
use clap::Parser;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long, env = "PORT", default_value_t = 50001)]
    port: u16,

    #[arg(long, env = "PORTV2", default_value_t = 50000)]
    portv2: u16,

    #[arg(long, env = "SERVER_NAME", default_value = "Unnamed server")]
    name: String,

    #[arg(long, env = "MAX_CLIENTS", default_value_t = 200)]
    maxclients: usize,

    #[arg(long, env = "NO_MASTER", default_value_t = false)]
    nomaster: bool,

    #[arg(long, env = "MASTER_PW", default_value = "")]
    masterpw: String,

    #[arg(long, env = "MAINT_PW", default_value = "test123456")]
    maintpw: String,

    #[arg(long, env = "MASTER_URL", default_value = "http://cncnet.org/master-announce")]
    master: String,

    #[arg(long, env = "IP_LIMIT", default_value_t = 8)]
    iplimit: usize,

    #[arg(long, env = "IP_LIMIT_V2", default_value_t = 4)]
    iplimitv2: usize,

    #[arg(long, env = "NO_P2P", default_value_t = false)]
    nop2p: bool,

    #[arg(long, env = "WORKER_THREADS", default_value_t = 0)]
    workers: usize,

    #[arg(long, env = "METRICS_PORT", default_value_t = 9090)]
    metrics_port: u16,

    #[arg(long, env = "LOG_LEVEL", default_value = "info")]
    log_level: String,

    #[arg(long, env = "LOG_FORMAT", default_value = "pretty")]
    log_format: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize tracing with configurable format
    init_tracing(&args.log_level, &args.log_format)?;

    info!("Starting CnCNet Server v2.0.0 - Production Edition");
    info!("Configuration loaded from CLI args and environment");

    // Validate and create config
    let config = Arc::new(config::ServerConfig::from_args(args)?);

    // Initialize global buffer pool
    buffer_pool::init_global_pool();

    // Initialize metrics
    let metrics = Arc::new(metrics::Metrics::new());

    // Create shutdown coordinator
    let shutdown = shutdown::ShutdownCoordinator::new();

    // Create task semaphore to prevent unbounded spawning
    let task_limiter = Arc::new(Semaphore::new(10000));

    // Start health check server
    let health_handle = health::start_health_server(
        config.metrics_port,
        metrics.clone(),
        shutdown.subscribe(),
    );

    // Start services
    let mut service_handles = Vec::new();

    // P2P services
    if !config.no_p2p {
        info!("Starting P2P services on ports 8054 and 3478");

        let p2p_8054 = peer_to_peer::PeerToPeer::new(
            8054,
            metrics.clone(),
        );
        let p2p_3478 = peer_to_peer::PeerToPeer::new(
            3478,
            metrics.clone(),
        );

        let mut shutdown_rx1 = shutdown.subscribe();
        service_handles.push(tokio::spawn(async move {
            tokio::select! {
                result = p2p_8054.start() => {
                    if let Err(e) = result {
                        error!("P2P 8054 error: {}", e);
                    }
                }
                _ = shutdown_rx1.recv() => {
                    info!("P2P 8054 shutting down");
                }
            }
        }));

        let mut shutdown_rx2 = shutdown.subscribe();
        service_handles.push(tokio::spawn(async move {
            tokio::select! {
                result = p2p_3478.start() => {
                    if let Err(e) = result {
                        error!("P2P 3478 error: {}", e);
                    }
                }
                _ = shutdown_rx2.recv() => {
                    info!("P2P 3478 shutting down");
                }
            }
        }));
    }

    // Tunnel V3
    {
        let tunnel_v3 = Arc::new(tunnel_v3::TunnelV3::new(
            config.clone(),
            metrics.clone(),
            task_limiter.clone(),
        ));
        info!("Starting Tunnel V3 on port {}", config.tunnel_port);

        let mut shutdown_rx = shutdown.subscribe();
        service_handles.push(tokio::spawn(async move {
            tokio::select! {
                result = tunnel_v3.start() => {
                    if let Err(e) = result {
                        error!("Tunnel V3 error: {}", e);
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Tunnel V3 shutting down");
                }
            }
        }));
    }

    // Tunnel V2
    {
        let tunnel_v2 = Arc::new(tunnel_v2::TunnelV2::new(
            config.clone(),
            metrics.clone(),
            task_limiter.clone(),
        ));
        info!("Starting Tunnel V2 on port {}", config.tunnel_v2_port);

        let mut shutdown_rx = shutdown.subscribe();
        service_handles.push(tokio::spawn(async move {
            tokio::select! {
                result = tunnel_v2.start() => {
                    if let Err(e) = result {
                        error!("Tunnel V2 error: {}", e);
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Tunnel V2 shutting down");
                }
            }
        }));
    }

    // Wait for shutdown signal
    shutdown_handler(shutdown.clone()).await?;

    info!("Shutting down gracefully...");

    // Signal all services to stop
    shutdown.shutdown().await;

    // Wait for services with timeout
    let timeout = tokio::time::timeout(
        Duration::from_secs(30),
        futures::future::join_all(service_handles),
    ).await;

    match timeout {
        Ok(_) => info!("All services stopped successfully"),
        Err(_) => warn!("Some services did not stop within timeout"),
    }

    // Stop health server
    health_handle.abort();

    info!("Shutdown complete");
    Ok(())
}

fn init_tracing(level: &str, format: &str) -> Result<()> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    let fmt_layer = match format {
        "json" => fmt::layer()
            .json()
            .with_target(false)
            .with_current_span(true)
            .boxed(),
        _ => fmt::layer()
            .with_target(false)
            .with_thread_ids(true)
            .with_line_number(true)
            .boxed(),
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .init();

    Ok(())
}

async fn shutdown_handler(coordinator: shutdown::ShutdownCoordinator) -> Result<()> {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received Ctrl+C signal");
        }
        _ = terminate => {
            info!("Received terminate signal");
        }
    }

    Ok(())
}