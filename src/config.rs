//! Configuration module for CnCNet server
//!
//! This module defines command-line options and configuration parameters
//! for the tunnel server.

use clap::Parser;

/// Command-line options for CnCNet server
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Options {
    /// Port used for the tunnel server
    #[arg(long, default_value_t = 50001)]
    pub port: u16,

    /// Port used for the V2 tunnel server
    #[arg(long, default_value_t = 50000)]
    pub portv2: u16,

    /// Name of the server
    #[arg(long, default_value = "Unnamed server")]
    pub name: String,

    /// Maximum clients allowed on the tunnel server
    #[arg(long, default_value_t = 200)]
    pub maxclients: usize,

    /// Don't register to master
    #[arg(long, default_value_t = false)]
    pub nomaster: bool,

    /// Master password
    #[arg(long, default_value = "")]
    pub masterpw: String,

    /// Maintenance password
    #[arg(long, default_value = "KUYn3b2z")]
    pub maintpw: String,

    /// Master server URL
    #[arg(long, default_value = "http://cncnet.org/master-announce")]
    pub master: String,

    /// Maximum clients allowed per IP address
    #[arg(long, default_value_t = 8)]
    pub iplimit: usize,

    /// Max game request allowed per IP address on V2 tunnel
    #[arg(long, default_value_t = 4)]
    pub iplimitv2: usize,

    /// Disable NAT traversal ports (8054, 3478 UDP)
    #[arg(long, default_value_t = false)]
    pub nop2p: bool,
}

impl Options {
    /// Validates and normalizes the configuration options
    pub fn validate(&mut self) {
        // Ensure port is not in privileged range
        if self.port <= 1024 {
            self.port = 50001;
        }
        if self.portv2 <= 1024 {
            self.portv2 = 50000;
        }

        // Ensure reasonable limits
        if self.maxclients < 2 {
            self.maxclients = 200;
        }
        if self.iplimit < 1 {
            self.iplimit = 8;
        }
        if self.iplimitv2 < 1 {
            self.iplimitv2 = 4;
        }

        // Clean server name (remove semicolons)
        if self.name.is_empty() {
            self.name = "Unnamed server".to_string();
        } else {
            self.name = self.name.replace(';', "");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_options_validation() {
        let mut opts = Options {
            port: 80,
            portv2: 443,
            name: "Test;Server".to_string(),
            maxclients: 1,
            nomaster: false,
            masterpw: String::new(),
            maintpw: String::new(),
            master: String::new(),
            iplimit: 0,
            iplimitv2: 0,
            nop2p: false,
        };

        opts.validate();

        assert_eq!(opts.port, 50001);
        assert_eq!(opts.portv2, 50000);
        assert_eq!(opts.name, "TestServer");
        assert_eq!(opts.maxclients, 200);
        assert_eq!(opts.iplimit, 8);
        assert_eq!(opts.iplimitv2, 4);
    }
}