use anyhow::{Result, bail};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub tunnel_port: u16,
    pub tunnel_v2_port: u16,
    pub name: String,
    pub max_clients: usize,
    pub no_master_announce: bool,
    pub master_password: String,
    pub maintenance_password: String,
    pub master_server_url: String,
    pub ip_limit: usize,
    pub ip_limit_v2: usize,
    pub no_p2p: bool,
    pub worker_threads: usize,
    pub metrics_port: u16,
}

impl ServerConfig {
    pub fn from_args(args: crate::Args) -> Result<Self> {
        // Validate ports
        if args.port <= 1024 && args.port != 0 {
            bail!("Port {} requires root privileges. Use a port > 1024 or 0 for auto", args.port);
        }

        if args.portv2 <= 1024 && args.portv2 != 0 {
            bail!("Port {} requires root privileges. Use a port > 1024 or 0 for auto", args.portv2);
        }

        // Validate limits
        if args.maxclients < 2 {
            bail!("Max clients must be at least 2");
        }

        if args.maxclients > 10000 {
            bail!("Max clients cannot exceed 10000 for safety");
        }

        Ok(Self {
            tunnel_port: args.port,
            tunnel_v2_port: args.portv2,
            name: args.name.replace(';', "").trim().to_string(),
            max_clients: args.maxclients,
            no_master_announce: args.nomaster,
            master_password: args.masterpw,
            maintenance_password: args.maintpw,
            master_server_url: args.master,
            ip_limit: args.iplimit.max(1).min(100),
            ip_limit_v2: args.iplimitv2.max(1).min(100),
            no_p2p: args.nop2p,
            worker_threads: args.workers,
            metrics_port: args.metrics_port,
        })
    }
}

pub type SharedConfig = Arc<ServerConfig>;