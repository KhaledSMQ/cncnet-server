//! Configuration module for CnCNet server
//!
//! This module defines command-line options and configuration parameters
//! for the tunnel server.

use clap::Parser;
use tracing::warn;

/// Command-line options for CnCNet server
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Options {
    /// Port used for the tunnel server
    #[arg(long, default_value_t = 50001, env = "CNCNET_PORT_V3")]
    pub port: u16,

    /// Port used for the V2 tunnel server
    #[arg(long, default_value_t = 50000, env = "CNCNET_PORT_V2")]
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
    ///
    /// This method ensures all configuration values are within acceptable ranges
    /// and logs any changes made to the user's specified values.
    pub fn validate(&mut self) {
        // Ensure port is not in privileged range
        if self.port <= 1024 {
            warn!(
                "Port {} is in privileged range (<=1024), changing to default 50001",
                self.port
            );
            self.port = 50001;
        }
        if self.portv2 <= 1024 {
            warn!(
                "Port V2 {} is in privileged range (<=1024), changing to default 50000",
                self.portv2
            );
            self.portv2 = 50000;
        }

        // Ensure reasonable limits
        if self.maxclients < 2 {
            warn!(
                "Max clients {} is too low (minimum 2), changing to default 200",
                self.maxclients
            );
            self.maxclients = 200;
        } else if self.maxclients > 10000 {
            warn!(
                "Max clients {} is very high (>10000), this may impact performance",
                self.maxclients
            );
        }

        if self.iplimit < 1 {
            warn!(
                "IP limit {} is too low (minimum 1), changing to default 8",
                self.iplimit
            );
            self.iplimit = 8;
        } else if self.iplimit > 100 {
            warn!(
                "IP limit {} is very high (>100), this may allow abuse",
                self.iplimit
            );
        }

        if self.iplimitv2 < 1 {
            warn!(
                "IP limit V2 {} is too low (minimum 1), changing to default 4",
                self.iplimitv2
            );
            self.iplimitv2 = 4;
        } else if self.iplimitv2 > 50 {
            warn!(
                "IP limit V2 {} is very high (>50), this may allow abuse",
                self.iplimitv2
            );
        }

        // Clean server name (remove semicolons)
        if self.name.is_empty() {
            warn!("Server name is empty, using default 'Unnamed server'");
            self.name = "Unnamed server".to_string();
        } else {
            let original_name = self.name.clone();
            self.name = self.name.replace(';', "");
            if original_name != self.name {
                warn!(
                    "Server name contained semicolons which were removed: '{}' -> '{}'",
                    original_name, self.name
                );
            }

            // Warn if name is very long
            if self.name.len() > 100 {
                warn!(
                    "Server name is very long ({} chars), this may cause issues with master server",
                    self.name.len()
                );
            }
        }

        // Validate master server URL
        if !self.nomaster && !self.master.is_empty() {
            if !self.master.starts_with("http://") && !self.master.starts_with("https://") {
                warn!(
                    "Master server URL '{}' doesn't start with http:// or https://",
                    self.master
                );
            }
        }

        // Check for port conflicts
        if self.port == self.portv2 {
            warn!(
                "V3 and V2 ports are the same ({}), this will cause binding conflicts!",
                self.port
            );
        }

        // Security warnings
        if self.maintpw.is_empty() {
            warn!("Maintenance password is empty, maintenance commands will be disabled");
        } else if self.maintpw == "KUYn3b2z" {
            warn!("Using default maintenance password, consider changing it for security");
        }

        if !self.nomaster && self.masterpw.is_empty() {
            warn!("Master password is empty, server may not be accepted by master");
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

    #[test]
    fn test_name_validation() {
        let mut opts = Options {
            port: 50001,
            portv2: 50000,
            name: "Server;With;Semicolons".to_string(),
            maxclients: 100,
            nomaster: false,
            masterpw: String::new(),
            maintpw: String::new(),
            master: String::new(),
            iplimit: 8,
            iplimitv2: 4,
            nop2p: false,
        };

        opts.validate();
        assert_eq!(opts.name, "ServerWithSemicolons");
    }

    #[test]
    fn test_port_conflict_detection() {
        let mut opts = Options {
            port: 50000,
            portv2: 50000,
            name: "Test".to_string(),
            maxclients: 100,
            nomaster: false,
            masterpw: String::new(),
            maintpw: String::new(),
            master: String::new(),
            iplimit: 8,
            iplimitv2: 4,
            nop2p: false,
        };

        // Should warn about port conflict but not change
        opts.validate();
        assert_eq!(opts.port, 50000);
        assert_eq!(opts.portv2, 50000);
    }

    #[test]
    fn test_high_limits_warning() {
        let mut opts = Options {
            port: 50001,
            portv2: 50000,
            name: "Test".to_string(),
            maxclients: 15000,
            nomaster: false,
            masterpw: String::new(),
            maintpw: String::new(),
            master: String::new(),
            iplimit: 150,
            iplimitv2: 75,
            nop2p: false,
        };

        // Should warn but not change high values
        opts.validate();
        assert_eq!(opts.maxclients, 15000);
        assert_eq!(opts.iplimit, 150);
        assert_eq!(opts.iplimitv2, 75);
    }
}