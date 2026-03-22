# CnCNet Tunnel Server

A high-performance, multi-protocol tunnel server for Command & Conquer: Red Alert 2 and Yuri's Revenge online
multiplayer gaming. This project is a Rust port of the original [CnCNet server](https://github.com/CnCNet/cncnet-server)
with significant performance optimizations and modern architecture.

## Features

- **High Performance**: Optimized UDP relay with per-worker batched metrics and lock-free data structures
- **Multi-Protocol Support**: TunnelV3 (high-performance, 32-bit client IDs) and TunnelV2 (backward compatibility, 16-bit client IDs)
- **STUN P2P Support**: Built-in STUN servers for peer-to-peer connections on ports 8054 and 3478
- **Real-time Metrics**: Prometheus metrics endpoint and health checks
- **ARM64 Optimized**: jemalloc allocator, CPU pinning, and tuned socket buffers on ARM64/Linux
- **Graceful Shutdown**: Clean shutdown with signal handling (SIGTERM, Ctrl+C)
- **Security Features**: Rate limiting, IP-based connection limits, maintenance mode
- **Docker Ready**: Multi-arch Docker configuration with optimized builds

## Installation

### Prerequisites

- Rust 1.75 or later
- Linux/Unix system (recommended for production)

### Building from Source

```bash
# Clone the repository
git clone https://github.com/khaledsmq/cncnet-server.git
cd cncnet-server

# Build the project
./build.sh

# Or manually with cargo
cargo build --release
```

### Using Docker

```bash
# Build the Docker image
docker build -t cncnet-server .

# Run the container
docker run -d \
  --name cncnet-server \
  -p 50000:50000/tcp \
  -p 50000:50000/udp \
  -p 50001:50001/udp \
  -p 8054:8054/udp \
  -p 3478:3478/udp \
  -p 9090:9090/tcp \
  cncnet-server
```

## Configuration

The server is configured via command-line arguments or environment variables:

```bash
./target/release/cncnet-server --help
```

| Option           | Environment Variable | Default                           | Description                                          |
| ---------------- | -------------------- | --------------------------------- | ---------------------------------------------------- |
| `--port`         | `PORT`               | 50001                             | TunnelV3 (main) UDP server port                      |
| `--portv2`       | `PORTV2`             | 50000                             | TunnelV2 (compatibility) UDP + HTTP server port      |
| `--name`         | `SERVER_NAME`        | "Unnamed server"                  | Server name for master server announcements          |
| `--maxclients`   | `MAX_CLIENTS`        | 200                               | Maximum concurrent clients (2–10000)                 |
| `--workers`      | `WORKER_THREADS`     | 0                                 | Tokio worker threads (0 = platform default; on ARM64 defaults to min(num_cpus, 4)) |
| `--iplimit`      | `IP_LIMIT`           | 8                                 | Max connections per IP (V3, clamped 1–100)           |
| `--iplimitv2`    | `IP_LIMIT_V2`        | 4                                 | Max connections per IP (V2, clamped 1–100)           |
| `--masterpw`     | `MASTER_PW`          | ""                                | Master server password                               |
| `--maintpw`      | `MAINT_PW`           | "test123456"                      | Maintenance mode password                            |
| `--master`       | `MASTER_URL`         | http://cncnet.org/master-announce | Master server URL                                    |
| `--nomaster`     | `NO_MASTER`          | false                             | Disable master server announcements                  |
| `--nop2p`        | `NO_P2P`             | false                             | Disable P2P STUN servers                             |
| `--metrics-port` | `METRICS_PORT`       | 9090                              | Health / Prometheus metrics HTTP port                 |
| `--log-level`    | `LOG_LEVEL`          | "info"                            | Logging level (trace/debug/info/warn/error)          |
| `--log-format`   | `LOG_FORMAT`         | "pretty"                          | Log format (pretty/json)                             |

## Quick Start

### Basic Usage

```bash
# Start with default settings
./target/release/cncnet-server

# Start with custom configuration
./target/release/cncnet-server \
  --name "My Server" \
  --maxclients 500 \
  --workers 4
```

### Systemd Deployment

A sample systemd unit file is provided in `cncnet.service`:

```bash
# Copy and edit the service file
sudo cp cncnet.service /etc/systemd/system/
sudo systemctl daemon-reload

# Start the service
sudo systemctl start cncnet-server
sudo systemctl enable cncnet-server
```

## Monitoring

### Health & Metrics Server (port 9090)

The server runs a dedicated HTTP health/metrics server on the `--metrics-port` (default 9090):

| Endpoint   | Description                        |
| ---------- | ---------------------------------- |
| `/health`  | Returns `OK` — basic liveness      |
| `/ready`   | Returns `READY` — readiness probe  |
| `/metrics` | Prometheus-format metrics export   |

### TunnelV2 HTTP Endpoints (port 50000)

The V2 tunnel also serves HTTP on its UDP port:

| Endpoint                 | Description                          |
| ------------------------ | ------------------------------------ |
| `/status`                | Slot usage (free / in-use / dropped) |
| `/health`                | Returns `OK`                         |
| `/request?clients=N`     | Allocate N client slots (2–8)        |
| `/maintenance/{password}`| Toggle maintenance mode              |

### Maintenance Mode

Toggle maintenance mode to reject new connections while keeping existing ones:

```bash
# Via V2 HTTP
curl http://localhost:50000/maintenance/your-password

# Via V3 UDP command packet (29+ bytes):
#   bytes 0-3:  sender_id = 0x00000000
#   bytes 4-7:  receiver_id = 0xFFFFFFFF
#   byte  8:    command (0 = toggle maintenance, 1 = force cleanup)
#   bytes 9-28: SHA-1 hash of maintenance password
```

## AWS EC2 Deployment

The server is optimized for AWS Graviton (ARM64) instances.

### One-Click Deploy (CloudFormation)

Launch a fully configured server on AWS with a single click. This provisions a Graviton EC2 instance,
security group, Elastic IP, and builds/starts the server automatically.

[![Launch Stack](https://s3.amazonaws.com/cloudformation-examples/cloudformation-launch-stack.png)](https://console.aws.amazon.com/cloudformation/home#/stacks/new?stackName=cncnet-server&templateURL=https://raw.githubusercontent.com/khaledsmq/cncnet-server/main/cloudformation/cncnet-server.yaml)

> **Prerequisites**: An AWS account and an existing [EC2 key pair](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/ec2-key-pairs.html) in the target region.

The stack wizard lets you configure:

| Parameter         | Default         | Description                                     |
| ----------------- | --------------- | ----------------------------------------------- |
| `InstanceType`    | t4g.medium      | ARM64/Graviton instance type                    |
| `KeyName`         | *(required)*    | EC2 key pair for SSH access                     |
| `ServerName`      | CnCNet Server   | Server name in the master server list           |
| `MaxClients`      | 200             | Maximum concurrent clients (2–10000)            |
| `V3Port`          | 50001           | TunnelV3 UDP port                               |
| `V2Port`          | 50000           | TunnelV2 UDP + HTTP port                        |
| `NoMaster`        | false           | Disable master server announcements             |
| `MetricsAccessCidr` | 0.0.0.0/0     | CIDR for metrics port access (restrict to your IP!) |
| `SSHAccessCidr`   | 0.0.0.0/0       | CIDR for SSH access (restrict to your IP!)      |
| `VolumeSize`      | 20              | Root EBS volume size in GiB                     |

After the stack completes (~15 min), check the **Outputs** tab for the server's Elastic IP, SSH command, and endpoint URLs.

You can also deploy via the AWS CLI:

```bash
aws cloudformation create-stack \
  --stack-name cncnet-server \
  --template-body file://cloudformation/cncnet-server.yaml \
  --parameters \
    ParameterKey=KeyName,ParameterValue=my-key \
    ParameterKey=ServerName,ParameterValue="My CnCNet Server" \
    ParameterKey=MaxClients,ParameterValue=200 \
    ParameterKey=SSHAccessCidr,ParameterValue="$(curl -s ifconfig.me)/32" \
    ParameterKey=MetricsAccessCidr,ParameterValue="$(curl -s ifconfig.me)/32" \
  --capabilities CAPABILITY_IAM \
  --region us-east-1
```

To tear down everything:

```bash
aws cloudformation delete-stack --stack-name cncnet-server --region us-east-1
```

### Recommended Instance

**t4g.medium** (2 vCPU, 4 GB RAM) — ARM64/Graviton2, excellent price-performance for game tunnel workloads.

### Option 1: Fresh Instance (Full Setup)

Use `scripts/aws_arm64_build.sh` for a fresh EC2 instance. It handles everything: system packages, Rust installation, building from source, systemd service, firewall, kernel tuning, fail2ban, log rotation, and health monitoring.

```bash
# SSH into your EC2 instance
ssh -i your-key.pem ec2-user@your-instance-ip

# Clone the repo
git clone https://github.com/khaledsmq/cncnet-server.git
cd cncnet-server

# Run the full setup (as root)
sudo SERVER_NAME="My Server" MAX_CLIENTS=200 bash scripts/aws_arm64_build.sh
```

The script accepts environment variables for configuration:

| Variable         | Default                           | Description                      |
| ---------------- | --------------------------------- | -------------------------------- |
| `SERVER_NAME`    | "CnCNet Server"                   | Server name for master announce  |
| `MAX_CLIENTS`    | 200                               | Maximum concurrent clients       |
| `V3_PORT`        | 50001                             | TunnelV3 UDP port                |
| `V2_PORT`        | 50000                             | TunnelV2 UDP + HTTP port         |
| `MASTER_URL`     | http://cncnet.org/master-announce | Master server URL                |
| `NO_MASTER`      | false                             | Disable master announcements     |
| `MAINT_PASSWORD` | (random)                          | Maintenance mode password        |
| `LOG_LEVEL`      | info                              | Log level                        |
| `FORCE_REINSTALL`| false                             | Set to `true` to overwrite existing install |

After installation:

```bash
# Check status
sudo systemctl status cncnet-server

# View logs
sudo journalctl -u cncnet-server -f

# Run health check
/opt/cncnet-server/health_check.sh
```

### Option 2: Amazon Linux (Build + Deploy)

Use `build-arm.sh` for Amazon Linux 2 or 2023. It detects Graviton processors, installs dependencies, builds with CPU-specific flags (`neoverse-n1`), applies kernel tuning via `99-cncnet-arm.conf`, and sets up systemd.

```bash
ssh -i your-key.pem ec2-user@your-instance-ip

git clone https://github.com/khaledsmq/cncnet-server.git
cd cncnet-server

# Build and install
./build-arm.sh
```

After installation:

```bash
# Monitor
/opt/cncnet/monitor.sh

# Logs
sudo journalctl -u cncnet -f

# Uninstall
sudo /opt/cncnet/uninstall.sh
```

### AWS Security Group

Regardless of which script you use, configure your EC2 Security Group to allow inbound traffic:

| Port  | Protocol | Source    | Purpose              |
| ----- | -------- | --------- | -------------------- |
| 50001 | UDP      | 0.0.0.0/0 | TunnelV3             |
| 50000 | TCP+UDP  | 0.0.0.0/0 | TunnelV2 (UDP + HTTP)|
| 8054  | UDP      | 0.0.0.0/0 | P2P STUN             |
| 3478  | UDP      | 0.0.0.0/0 | P2P STUN             |
| 9090  | TCP      | Your IP   | Metrics (restrict!)  |
| 22    | TCP      | Your IP   | SSH                  |

### Using Docker on EC2

Alternatively, deploy with Docker on any EC2 instance:

```bash
# Install Docker (Amazon Linux 2023)
sudo dnf install -y docker
sudo systemctl enable --now docker

# Build and run
git clone https://github.com/khaledsmq/cncnet-server.git
cd cncnet-server

docker build -t cncnet-server .
docker run -d \
  --name cncnet-server \
  --restart unless-stopped \
  --network host \
  cncnet-server \
  cncnet-server --name "My Server" --maxclients 200
```

Using `--network host` avoids Docker NAT overhead, which matters for UDP-heavy workloads.

## System Optimization

### Recommended System Settings

The `build-arm.sh` and `scripts/aws_arm64_build.sh` scripts apply these automatically. For manual setup:

```bash
# Network buffer settings
sudo sysctl -w net.core.rmem_max=134217728
sudo sysctl -w net.core.wmem_max=134217728
sudo sysctl -w net.core.netdev_max_backlog=5000
sudo sysctl -w net.core.somaxconn=65535

# File descriptor limits
ulimit -n 1048576
echo '* soft nofile 1048576' >> /etc/security/limits.conf
echo '* hard nofile 1048576' >> /etc/security/limits.conf

# Or apply the full ARM-tuned sysctl config
sudo cp 99-cncnet-arm.conf /etc/sysctl.d/
sudo sysctl -p /etc/sysctl.d/99-cncnet-arm.conf
```

## Architecture

### Protocol Support

- **TunnelV3**: High-performance protocol with 32-bit client IDs (UDP only, auto-registration)
- **TunnelV2**: Legacy protocol with 16-bit client IDs (UDP + HTTP for slot allocation)
- **STUN P2P**: STUN servers on ports 8054 and 3478 for peer-to-peer NAT traversal

### Performance Features

- **Lock-free data structures**: DashMap and atomic operations for concurrent access
- **Batched metrics**: Per-worker `LocalMetricsBatch` to reduce atomic contention on the hot path
- **Worker threads**: Multiple SO_REUSEPORT UDP receive workers per service
- **Rate limiting**: Per-IP rate limiting with periodic reset
- **Memory allocator**: jemalloc on ARM64/Linux for reduced fragmentation; system allocator elsewhere

### Project Structure

```
src/
├── main.rs          # Entry point, CLI args, runtime setup
├── config.rs        # ServerConfig from CLI args
├── tunnel_v3.rs     # High-performance TunnelV3 server (UDP)
├── tunnel_v2.rs     # Legacy TunnelV2 server (UDP + HTTP)
├── peer_to_peer.rs  # STUN P2P servers
├── metrics.rs       # Atomic counters + Prometheus export
├── health.rs        # Health/readiness/metrics HTTP server
├── rate_limiter.rs  # Per-IP rate limiting
├── shutdown.rs      # Graceful shutdown coordination
└── errors.rs        # Error types
```

## Development

### Building

```bash
# Development build
cargo build

# Release build with optimizations
cargo build --release

# Run tests
cargo test

# Run with debug logging
RUST_LOG=debug cargo run
```

## Security

- **Rate limiting**: Configurable per-IP connection and ping limits
- **Input validation**: Strict packet size and address validation
- **Memory safety**: Rust's ownership and borrowing guarantees
- **Connection limits**: Per-IP caps enforced by `ConnectionRateLimiter` (V3) and `RateLimiter` (V2/P2P)
- **Maintenance mode**: Password-protected via SHA-1 (V3 UDP) or URL path (V2 HTTP)

## Troubleshooting

### Common Issues

1. **Port binding errors**: Ports below 1024 require root privileges (the server rejects them by default)
2. **High memory usage**: Reduce `--maxclients` (hard cap: 10000)
3. **Packet drops**: Increase socket buffer sizes via sysctl
4. **Connection limits**: Verify system ulimits (`ulimit -n`)

### Logging

```bash
# Enable debug logging
RUST_LOG=debug ./cncnet-server

# JSON-formatted logs
./cncnet-server --log-format json

# Log to file
./cncnet-server 2>&1 | tee server.log
```

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Credits

This project is a high-performance Rust port of the original [CnCNet server](https://github.com/CnCNet/cncnet-server).
Special thanks to the CnCNet team for their foundational work on the original server implementation.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request. For major changes, please open an issue first to
discuss what you would like to change.

### Development Setup

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests if applicable
5. Submit a pull request

---

**Built with Rust for the Command & Conquer community**
