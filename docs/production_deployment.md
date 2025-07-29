# CnCNet Server Production Deployment Guide

This guide provides comprehensive instructions for deploying the CnCNet server in production environments on AWS ARM64 instances.

## Table of Contents

1. [System Requirements](#system-requirements)
2. [Quick Start](#quick-start)
3. [Deployment Options](#deployment-options)
4. [Configuration](#configuration)
5. [Monitoring](#monitoring)
6. [Maintenance](#maintenance)
7. [Troubleshooting](#troubleshooting)
8. [Performance Tuning](#performance-tuning)

## System Requirements

### Hardware Requirements
- **Architecture**: ARM64 (AWS Graviton2/3)
- **CPU**: Minimum 2 vCPUs, recommended 4+ vCPUs
- **RAM**: Minimum 2GB, recommended 4GB+
- **Storage**: 20GB+ SSD
- **Network**: 1Gbps+ connection

### Recommended AWS Instance Types
- **Small deployments**: t4g.medium (2 vCPU, 4GB RAM)
- **Medium deployments**: m6g.large (2 vCPU, 8GB RAM)
- **Large deployments**: c6g.xlarge (4 vCPU, 8GB RAM)

### Software Requirements
- Amazon Linux 2023, Ubuntu 22.04 LTS, or Debian 12
- Rust 1.75+ (installed automatically)
- systemd-based init system

## Quick Start

### Option 1: Automated Script Deployment

```bash
# Download and run the installation script
curl -sSL https://raw.githubusercontent.com/khaledsmq/cncnet-server/main/scripts/aws_arm64_build.sh -o install.sh
chmod +x install.sh

# Run with default settings
sudo ./install.sh

# Or with custom settings
sudo SERVER_NAME="[0 UAE] Dubai" MAX_CLIENTS=16 ./install.sh
```

### Option 2: CloudFormation Deployment

```bash
# Deploy using AWS CLI
aws cloudformation create-stack \
  --stack-name cncnet-server \
  --template-body file://cloudformation.yaml \
  --parameters \
    ParameterKey=InstanceType,ParameterValue=t4g.medium \
    ParameterKey=KeyName,ParameterValue=my-key-pair \
    ParameterKey=ServerName,ParameterValue="Production Server" \
  --capabilities CAPABILITY_IAM
```

### Option 3: Docker Deployment

```bash
# Clone the repository
git clone https://github.com/khaledsmq/cncnet-server
cd cncnet-server

# Build and run with Docker Compose
docker-compose up -d
```

## Configuration

### Environment Variables

The server can be configured using environment variables:

```bash
# Server identification
SERVER_NAME="My CnCNet Server"

# Network ports
V3_PORT=50001          # Tunnel V3 UDP port
V2_PORT=50000          # Tunnel V2 UDP/TCP port

# Capacity settings
MAX_CLIENTS=1000       # Maximum concurrent clients
IP_LIMIT_V3=8          # Max clients per IP (V3)
IP_LIMIT_V2=4          # Max clients per IP (V2)

# Master server
MASTER_URL="http://cncnet.org/master-announce"
NO_MASTER=false        # Set to true to disable master announcements

# Security
MAINT_PASSWORD="secure_password"  # Maintenance mode password

# Logging
LOG_LEVEL=info         # Options: trace, debug, info, warn, error
```

### Configuration File

The server configuration is stored at `/etc/cncnet-server/server.conf`:

```bash
# Edit configuration
sudo nano /etc/cncnet-server/server.conf

# Restart server after changes
sudo systemctl restart cncnet-server
```

### Network Ports

Ensure the following ports are open in your security group / firewall:

| Port | Protocol | Purpose |
|------|----------|---------|
| 50001 | UDP | Tunnel V3 game traffic |
| 50000 | UDP | Tunnel V2 game traffic |
| 50000 | TCP | Tunnel V2 HTTP API |
| 8054 | UDP | P2P NAT traversal |
| 3478 | UDP | STUN protocol |
| 22 | TCP | SSH access |
| 3000 | TCP | Grafana (optional) |

## Monitoring

### Service Status

```bash
# Check service status
sudo systemctl status cncnet-server

# View live logs
sudo journalctl -u cncnet-server -f

# Check health
/opt/cncnet-server/health_check.sh
```

### Metrics and Dashboards

The server exposes metrics that can be visualized using the included Grafana dashboard:

1. Access Grafana at `http://your-server-ip:3000`
2. Default credentials: admin/admin
3. Import the dashboard from `/grafana/dashboards/cncnet.json`

### CloudWatch Integration

If deployed on AWS, metrics are automatically sent to CloudWatch:
- Namespace: `CnCNet`
- Log Group: `/aws/ec2/cncnet`

### Key Metrics to Monitor

- **CPU Usage**: Should stay below 80%
- **Memory Usage**: Should stay below 90%
- **Active Connections**: Monitor against max_clients
- **Packet Rate**: Watch for sudden spikes (DDoS)
- **Dropped Packets**: Should be minimal (<1%)

## Maintenance

### Regular Updates

```bash
# Update system packages
sudo apt update && sudo apt upgrade -y  # Ubuntu/Debian
sudo yum update -y                       # Amazon Linux

# Update CnCNet server
cd /tmp
git clone https://github.com/khaledsmq/cncnet-server
cd cncnet-server
cargo build --release --target aarch64-unknown-linux-gnu
sudo systemctl stop cncnet-server
sudo cp target/aarch64-unknown-linux-gnu/release/cncnet-server /opt/cncnet-server/
sudo systemctl start cncnet-server
```

### Backup Configuration

```bash
# Backup script (add to cron)
#!/bin/bash
BACKUP_DIR="/backup/cncnet"
mkdir -p $BACKUP_DIR
tar -czf $BACKUP_DIR/cncnet-config-$(date +%Y%m%d).tar.gz /etc/cncnet-server/
tar -czf $BACKUP_DIR/cncnet-logs-$(date +%Y%m%d).tar.gz /var/log/cncnet-server/
find $BACKUP_DIR -name "*.tar.gz" -mtime +7 -delete

# Add to crontab
echo "0 2 * * * /opt/cncnet-server/backup.sh" | sudo crontab -
```

### Log Rotation

Logs are automatically rotated by logrotate. Configuration is at `/etc/logrotate.d/cncnet-server`.

### Maintenance Mode

```bash
# Enable maintenance mode (blocks new connections)
curl -X POST http://localhost:50001/maintenance/YOUR_MAINT_PASSWORD

# Disable maintenance mode
curl -X POST http://localhost:50001/maintenance/YOUR_MAINT_PASSWORD
```

## Troubleshooting

### Common Issues

#### Service Won't Start

```bash
# Check logs for errors
sudo journalctl -u cncnet-server -n 100

# Verify binary permissions
ls -la /opt/cncnet-server/cncnet-server

# Check port availability
sudo ss -tulnp | grep -E '50001|50000|8054|3478'
```

#### High CPU Usage

```bash
# Check for DDoS
sudo tcpdump -i any -c 1000 -nn port 50001 | awk '{print $3}' | cut -d. -f1-4 | sort | uniq -c | sort -nr | head -20

# Review rate limiting
grep "Rate limit" /var/log/cncnet-server/server.log | tail -100

# Adjust rate limits if needed
sudo nano /etc/cncnet-server/server.conf
sudo systemctl restart cncnet-server
```

#### Connection Issues

```bash
# Test UDP connectivity
nc -u -v your-server-ip 50001

# Check firewall rules
sudo iptables -L -n -v
sudo ufw status verbose  # Ubuntu

# Verify master server registration
curl http://cncnet.org/api/v1/servers | grep your-server-name
```

### Debug Mode

```bash
# Enable debug logging temporarily
sudo systemctl stop cncnet-server
sudo RUST_LOG=debug /opt/cncnet-server/cncnet-server --port 50001

# Or update systemd service
sudo systemctl edit cncnet-server
# Add: Environment="RUST_LOG=debug"
sudo systemctl restart cncnet-server
```

## Performance Tuning

### Network Optimization

```bash
# Apply network tuning (included in install script)
sudo sysctl -w net.core.rmem_max=134217728
sudo sysctl -w net.core.wmem_max=134217728
sudo sysctl -w net.ipv4.udp_rmem_min=8192
sudo sysctl -w net.ipv4.udp_wmem_min=8192
sudo sysctl -w net.core.netdev_max_backlog=30000
```

### CPU Optimization

```bash
# Set CPU governor to performance
echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor

# Pin server to specific CPUs (optional)
sudo taskset -c 0-3 systemctl restart cncnet-server
```

### Memory Optimization

```bash
# Disable swap for better performance
sudo swapoff -a
sudo sed -i '/ swap / s/^/#/' /etc/fstab

# Enable transparent huge pages
echo always | sudo tee /sys/kernel/mm/transparent_hugepage/enabled
```

### Connection Limits

Adjust based on your server capacity:

```bash
# Small server (t4g.medium)
MAX_CLIENTS=500
IP_LIMIT_V3=4
IP_LIMIT_V2=2

# Medium server (m6g.large)
MAX_CLIENTS=1000
IP_LIMIT_V3=8
IP_LIMIT_V2=4

# Large server (c6g.xlarge)
MAX_CLIENTS=2000
IP_LIMIT_V3=16
IP_LIMIT_V2=8
```

## Security Best Practices

### Firewall Configuration

```bash
# Strict firewall rules (only allow necessary ports)
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 22/tcp comment 'SSH'
sudo ufw allow 50001/udp comment 'CnCNet V3'
sudo ufw allow 50000 comment 'CnCNet V2'
sudo ufw allow 8054/udp comment 'P2P'
sudo ufw allow 3478/udp comment 'STUN'
sudo ufw enable
```

### DDoS Protection

```bash
# Enable SYN cookies
sudo sysctl -w net.ipv4.tcp_syncookies=1

# Configure fail2ban (auto-installed)
sudo fail2ban-client status cncnet
sudo fail2ban-client set cncnet bantime 3600
sudo fail2ban-client set cncnet maxretry 5
```

### Regular Security Updates

```bash
# Create update script
cat > /opt/cncnet-server/security-update.sh << 'EOF'
#!/bin/bash
apt update
apt upgrade -y
systemctl restart cncnet-server
EOF

# Schedule weekly updates
echo "0 3 * * 0 /opt/cncnet-server/security-update.sh" | sudo crontab -
```

## Scaling Strategies

### Horizontal Scaling

For very large deployments, use multiple servers behind a load balancer:

1. Deploy multiple instances using the CloudFormation template
2. Use AWS Network Load Balancer for UDP traffic
3. Configure session affinity based on client IP

### Geographic Distribution

Deploy servers in multiple regions for better latency:

```bash
# US East
aws cloudformation create-stack --region us-east-1 ...

# EU West
aws cloudformation create-stack --region eu-west-1 ...

# Asia Pacific
aws cloudformation create-stack --region ap-southeast-1 ...
```

## Support and Resources

- **GitHub Issues**: https://github.com/khaledsmq/cncnet-server/issues
- **CnCNet Forums**: https://forums.cncnet.org/
- **Documentation**: https://github.com/khaledsmq/cncnet-server/wiki

## License

This software is released under the MIT License. See LICENSE file for details.

---

**Author**: Khaled Sameer <khaled.smq@hotmail.com>  
**Repository**: https://github.com/khaledsmq/cncnet-server