#!/bin/bash
# Build and deployment script for CnCNet server on ARM64/AWS Graviton2
# For Amazon Linux 2/2023 on t4g.medium

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
PROJECT_NAME="cncnet-server"
INSTALL_DIR="/opt/cncnet"
SERVICE_NAME="cncnet"
BUILD_TYPE="${1:-release}"

echo -e "${GREEN}=== CnCNet Server ARM Build Script (Amazon Linux) ===${NC}"

# Detect Amazon Linux version
if [[ -f /etc/system-release ]]; then
    if grep -q "Amazon Linux 2023" /etc/system-release; then
        AL_VERSION="2023"
        PKG_MANAGER="dnf"
        echo -e "${GREEN}Detected Amazon Linux 2023${NC}"
    elif grep -q "Amazon Linux 2" /etc/system-release; then
        AL_VERSION="2"
        PKG_MANAGER="yum"
        echo -e "${GREEN}Detected Amazon Linux 2${NC}"
    else
        AL_VERSION="unknown"
        PKG_MANAGER="yum"
        echo -e "${YELLOW}Unknown Amazon Linux version${NC}"
    fi
else
    echo -e "${YELLOW}Not running on Amazon Linux${NC}"
    PKG_MANAGER="yum"
fi

# Check if running on ARM64
if [[ $(uname -m) != "aarch64" ]]; then
    echo -e "${YELLOW}Warning: Not running on ARM64 architecture ($(uname -m))${NC}"
    read -p "Continue anyway? (y/n) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

# Check for AWS Graviton2
if [[ -f /proc/cpuinfo ]] && grep -q "Neoverse" /proc/cpuinfo; then
    echo -e "${GREEN}✓ Detected AWS Graviton processor${NC}"
    GRAVITON=true
else
    echo -e "${YELLOW}! Not running on AWS Graviton${NC}"
    GRAVITON=false
fi

# Install dependencies based on Amazon Linux version
echo -e "${GREEN}Installing build dependencies...${NC}"

if [[ "$AL_VERSION" == "2023" ]]; then
    # Amazon Linux 2023
    sudo dnf update -y
    sudo dnf groupinstall -y "Development Tools"
    sudo dnf install -y \
        gcc \
        gcc-c++ \
        make \
        openssl-devel \
        pkg-config \
        lld \
        clang \
        jemalloc-devel \
        curl \
        git \
        perf
else
    # Amazon Linux 2
    sudo yum update -y
    sudo yum groupinstall -y "Development Tools"
    sudo yum install -y \
        gcc \
        gcc-c++ \
        make \
        openssl-devel \
        pkgconfig \
        clang \
        curl \
        git \
        perf

    # Install jemalloc from source on AL2 (not in default repos)
    if ! ldconfig -p | grep -q jemalloc; then
        echo -e "${GREEN}Building jemalloc from source...${NC}"
        cd /tmp
        wget https://github.com/jemalloc/jemalloc/releases/download/5.3.0/jemalloc-5.3.0.tar.bz2
        tar -xjf jemalloc-5.3.0.tar.bz2
        cd jemalloc-5.3.0
        ./configure --prefix=/usr/local
        make -j$(nproc)
        sudo make install
        sudo ldconfig
        cd -
        rm -rf /tmp/jemalloc*
    fi
fi

# Install LLD if not available
if ! command -v lld &> /dev/null; then
    echo -e "${GREEN}Installing LLD...${NC}"
    if [[ "$AL_VERSION" == "2023" ]]; then
        sudo dnf install -y lld
    else
        # Build LLD from LLVM for AL2
        sudo yum install -y llvm-toolset-7-lld 2>/dev/null || {
            echo -e "${YELLOW}LLD not available, using default linker${NC}"
        }
    fi
fi

# Install Rust if not present
if ! command -v cargo &> /dev/null; then
    echo -e "${GREEN}Installing Rust...${NC}"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    source $HOME/.cargo/env

    # Add aarch64 target
    rustup target add aarch64-unknown-linux-gnu
fi

# Ensure we have the right target
rustup target add aarch64-unknown-linux-gnu

# Set ARM-specific build flags
if [[ "$GRAVITON" == true ]]; then
    export RUSTFLAGS="-C target-cpu=neoverse-n1 -C target-feature=+crc,+aes,+sha2,+fp16,+rcpc,+dotprod,+crypto"

    # Use LLD if available
    if command -v lld &> /dev/null; then
        export RUSTFLAGS="$RUSTFLAGS -C link-arg=-fuse-ld=lld"
    fi

    echo -e "${GREEN}Using Graviton2-specific optimizations${NC}"
else
    export RUSTFLAGS="-C target-cpu=native"
    echo -e "${GREEN}Using native CPU optimizations${NC}"
fi

# Additional optimizations for release builds
if [[ "$BUILD_TYPE" == "release" ]]; then
    export RUSTFLAGS="$RUSTFLAGS -C lto=fat -C opt-level=3 -C codegen-units=1"
fi

# Set jemalloc path based on installation
if [[ -f /usr/local/lib/libjemalloc.so ]]; then
    export JEMALLOC_LIB_DIR=/usr/local/lib
    export LD_LIBRARY_PATH=/usr/local/lib:$LD_LIBRARY_PATH
elif [[ -f /usr/lib64/libjemalloc.so ]]; then
    export JEMALLOC_LIB_DIR=/usr/lib64
fi

# Build the project
echo -e "${GREEN}Building $PROJECT_NAME ($BUILD_TYPE)...${NC}"
if [[ "$BUILD_TYPE" == "release" ]]; then
    cargo build --release --target aarch64-unknown-linux-gnu
    BINARY_PATH="target/aarch64-unknown-linux-gnu/release/$PROJECT_NAME"
else
    cargo build --target aarch64-unknown-linux-gnu
    BINARY_PATH="target/aarch64-unknown-linux-gnu/debug/$PROJECT_NAME"
fi

# Strip binary for release
if [[ "$BUILD_TYPE" == "release" ]]; then
    echo -e "${GREEN}Stripping binary...${NC}"
    strip "$BINARY_PATH" 2>/dev/null || echo "Strip not available"
fi

# Check binary size
BINARY_SIZE=$(du -h "$BINARY_PATH" | cut -f1)
echo -e "${GREEN}Binary size: $BINARY_SIZE${NC}"

# Create installation directory
echo -e "${GREEN}Setting up installation directory...${NC}"
sudo mkdir -p $INSTALL_DIR/logs
sudo mkdir -p $INSTALL_DIR/cores

# Create cncnet user if it doesn't exist
if ! id -u cncnet &>/dev/null; then
    echo -e "${GREEN}Creating cncnet user...${NC}"
    sudo useradd -r -s /sbin/nologin -d $INSTALL_DIR cncnet
fi

# Copy binary
echo -e "${GREEN}Installing binary...${NC}"
sudo cp "$BINARY_PATH" "$INSTALL_DIR/"
sudo chown cncnet:cncnet "$INSTALL_DIR/$PROJECT_NAME"
sudo chmod +x "$INSTALL_DIR/$PROJECT_NAME"

# Create systemd service file
echo -e "${GREEN}Creating systemd service...${NC}"
cat << 'EOF' | sudo tee /etc/systemd/system/cncnet.service
[Unit]
Description=CnCNet Game Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=cncnet
Group=cncnet
WorkingDirectory=/opt/cncnet

# Environment variables
Environment="RUST_LOG=info"
Environment="RUST_BACKTRACE=1"
Environment="TOKIO_WORKER_THREADS=2"

# Jemalloc configuration
Environment="LD_PRELOAD=/usr/local/lib/libjemalloc.so.2"
Environment="MALLOC_CONF=background_thread:true,dirty_decay_ms:5000,muzzy_decay_ms:10000"

# Resource limits for t4g.medium
MemoryMax=3.5G
MemoryHigh=3G
CPUQuota=190%
TasksMax=4096

# Performance
LimitNOFILE=1048576
LimitNPROC=32768

# Security
NoNewPrivileges=true
PrivateTmp=true

# Restart policy
Restart=always
RestartSec=5
StartLimitBurst=3
StartLimitInterval=60

ExecStart=/opt/cncnet/cncnet-server \
    --port 50001 \
    --portv2 50000 \
    --name "Dubai" \
    --maxclients 200 \
    --iplimit 8 \
    --iplimitv2 4 \
    --workers 2 \
    --metrics-port 9090 \
    --log-level info

[Install]
WantedBy=multi-user.target
EOF

# Update jemalloc path in service file if needed
if [[ -f /usr/lib64/libjemalloc.so.2 ]]; then
    sudo sed -i 's|/usr/local/lib/libjemalloc.so.2|/usr/lib64/libjemalloc.so.2|' /etc/systemd/system/cncnet.service
elif [[ ! -f /usr/local/lib/libjemalloc.so.2 ]]; then
    # Remove jemalloc preload if not available
    sudo sed -i '/LD_PRELOAD/d' /etc/systemd/system/cncnet.service
    echo -e "${YELLOW}jemalloc not found, running without memory allocator optimization${NC}"
fi

sudo systemctl daemon-reload

# Apply kernel optimizations
echo -e "${GREEN}Applying kernel optimizations...${NC}"
cat << 'EOF' | sudo tee /etc/sysctl.d/99-cncnet-arm.conf
# CnCNet Server ARM Optimizations
net.core.rmem_max = 134217728
net.core.wmem_max = 134217728
net.core.rmem_default = 16777216
net.core.wmem_default = 16777216
net.core.netdev_max_backlog = 16384
net.core.netdev_budget = 600
net.core.somaxconn = 65535

# UDP tuning
net.ipv4.udp_mem = 16384 174760 134217728
net.ipv4.udp_rmem_min = 16384
net.ipv4.udp_wmem_min = 16384

# TCP tuning
net.ipv4.tcp_rmem = 4096 87380 134217728
net.ipv4.tcp_wmem = 4096 65536 134217728
net.ipv4.tcp_congestion_control = bbr
net.ipv4.tcp_fastopen = 3

# Connection tracking
net.netfilter.nf_conntrack_max = 262144
net.netfilter.nf_conntrack_udp_timeout = 30

# IP settings
net.ipv4.ip_local_port_range = 1024 65535
net.ipv4.tcp_syncookies = 1
net.ipv4.tcp_max_tw_buckets = 2000000
net.ipv4.tcp_tw_reuse = 1
net.ipv4.tcp_fin_timeout = 15

# VM settings for ARM
vm.swappiness = 10
vm.dirty_ratio = 15
vm.dirty_background_ratio = 5

# File limits
fs.file-max = 2097152
fs.nr_open = 1048576
EOF

sudo sysctl -p /etc/sysctl.d/99-cncnet-arm.conf

# Set up log rotation
echo -e "${GREEN}Setting up log rotation...${NC}"
cat << 'EOF' | sudo tee /etc/logrotate.d/cncnet
/opt/cncnet/logs/*.log {
    daily
    missingok
    rotate 7
    compress
    delaycompress
    notifempty
    create 0640 cncnet cncnet
    sharedscripts
    postrotate
        systemctl reload cncnet 2>/dev/null || true
    endscript
}
EOF

# Enable RPS (Receive Packet Steering)
if [[ "$GRAVITON" == true ]]; then
    echo -e "${GREEN}Configuring RPS for network interfaces...${NC}"
    for interface in $(ls /sys/class/net/ | grep -v lo); do
        echo 3 | sudo tee /sys/class/net/$interface/queues/rx-*/rps_cpus > /dev/null 2>&1 || true
    done
fi

# Set CPU governor to performance (if available)
if [[ -f /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor ]]; then
    echo -e "${GREEN}Setting CPU governor to performance...${NC}"
    echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor > /dev/null 2>&1 || true
fi

# Configure firewall (if firewalld is installed)
if command -v firewall-cmd &> /dev/null; then
    echo -e "${GREEN}Configuring firewall...${NC}"
    sudo firewall-cmd --permanent --add-port=50000/tcp
    sudo firewall-cmd --permanent --add-port=50000/udp
    sudo firewall-cmd --permanent --add-port=50001/udp
    sudo firewall-cmd --permanent --add-port=8054/udp
    sudo firewall-cmd --permanent --add-port=3478/udp
    sudo firewall-cmd --permanent --add-port=9090/tcp
    sudo firewall-cmd --reload
fi

# Configure iptables (if no firewalld)
#if ! command -v firewall-cmd &> /dev/null && command -v iptables &> /dev/null; then
#    echo -e "${GREEN}Configuring iptables...${NC}"
#    sudo iptables -A INPUT -p tcp --dport 50000 -j ACCEPT
#    sudo iptables -A INPUT -p udp --dport 50000 -j ACCEPT
#    sudo iptables -A INPUT -p udp --dport 50001 -j ACCEPT
#    sudo iptables -A INPUT -p udp --dport 8054 -j ACCEPT
#    sudo iptables -A INPUT -p udp --dport 3478 -j ACCEPT
#    sudo iptables -A INPUT -p tcp --dport 9090 -j ACCEPT
#
#    # Save iptables rules
#    if command -v iptables-save &> /dev/null; then
#        sudo iptables-save > /etc/sysconfig/iptables 2>/dev/null || true
#    fi
#fi

# Create environment file
echo -e "${GREEN}Creating environment configuration...${NC}"
cat << 'EOF' | sudo tee $INSTALL_DIR/.env
# CnCNet Server Configuration
PORT=50001
PORTV2=50000
SERVER_NAME=Dubai
MAX_CLIENTS=200
IP_LIMIT=8
IP_LIMIT_V2=4
WORKER_THREADS=2
METRICS_PORT=9090
LOG_LEVEL=info
EOF

# Set ownership
sudo chown -R cncnet:cncnet $INSTALL_DIR

# Enable and start service
echo -e "${GREEN}Starting $SERVICE_NAME service...${NC}"
sudo systemctl enable $SERVICE_NAME
sudo systemctl restart $SERVICE_NAME

# Wait for service to start
sleep 3

# Check service status
if sudo systemctl is-active --quiet $SERVICE_NAME; then
    echo -e "${GREEN}✓ Service started successfully!${NC}"

    # Show brief status
    sudo systemctl status $SERVICE_NAME --no-pager | head -n 10

    # Test endpoints
    echo -e "\n${GREEN}Testing endpoints...${NC}"

    # Health check
    if curl -s -o /dev/null -w "%{http_code}" http://localhost:9090/health 2>/dev/null | grep -q "200"; then
        echo -e "${GREEN}✓ Health endpoint: OK${NC}"
    else
        echo -e "${YELLOW}⚠ Health endpoint: Not responding yet${NC}"
    fi

    # Metrics
    if curl -s http://localhost:9090/metrics 2>/dev/null | grep -q "cncnet"; then
        echo -e "${GREEN}✓ Metrics endpoint: OK${NC}"
    else
        echo -e "${YELLOW}⚠ Metrics endpoint: Not ready yet${NC}"
    fi

    # V2 Status
    if curl -s http://localhost:50000/status 2>/dev/null | grep -q "slots"; then
        echo -e "${GREEN}✓ V2 Status endpoint: OK${NC}"
    else
        echo -e "${YELLOW}⚠ V2 Status endpoint: Not ready yet${NC}"
    fi

    echo -e "\n${GREEN}=== Installation Complete ===${NC}"
    echo -e "Server is running on:"
    echo -e "  • Port 50000 (Tunnel v2 - TCP/UDP)"
    echo -e "  • Port 50001 (Tunnel v3 - UDP)"
    echo -e "  • Port 8054 (P2P - UDP)"
    echo -e "  • Port 3478 (P2P - UDP)"
    echo -e "  • Port 9090 (Metrics - TCP)"
    echo -e "\nLogs: sudo journalctl -u $SERVICE_NAME -f"
else
    echo -e "${RED}✗ Service failed to start${NC}"
    sudo journalctl -u $SERVICE_NAME -n 50 --no-pager
    exit 1
fi

# Create monitoring script
cat << 'EOF' | sudo tee $INSTALL_DIR/monitor.sh > /dev/null
#!/bin/bash
echo "=== CnCNet Server Monitor ==="
echo "CPU & Memory:"
top -b -n 1 | head -5
echo ""
echo "Network Connections:"
ss -tunap | grep -E '5000[01]|8054|3478' | wc -l
echo ""
echo "Service Status:"
systemctl is-active cncnet
echo ""
echo "Recent Logs:"
journalctl -u cncnet -n 5 --no-pager
echo ""
echo "Metrics Summary:"
curl -s localhost:9090/metrics 2>/dev/null | grep -E 'cncnet_v[23]_active_clients|cncnet_dropped_packets_total' | head -5
EOF

sudo chmod +x $INSTALL_DIR/monitor.sh
sudo chown cncnet:cncnet $INSTALL_DIR/monitor.sh

# Create uninstall script
cat << 'EOF' | sudo tee $INSTALL_DIR/uninstall.sh > /dev/null
#!/bin/bash
echo "Stopping and removing CnCNet server..."
sudo systemctl stop cncnet
sudo systemctl disable cncnet
sudo rm -f /etc/systemd/system/cncnet.service
sudo rm -f /etc/sysctl.d/99-cncnet-arm.conf
sudo rm -f /etc/logrotate.d/cncnet
sudo rm -rf /opt/cncnet
sudo userdel cncnet 2>/dev/null || true
echo "CnCNet server uninstalled."
EOF

sudo chmod +x $INSTALL_DIR/uninstall.sh

# Performance tips
echo -e "\n${YELLOW}=== Quick Commands ===${NC}"
echo "• Monitor: $INSTALL_DIR/monitor.sh"
echo "• Logs: sudo journalctl -u cncnet -f"
echo "• Status: sudo systemctl status cncnet"
echo "• Restart: sudo systemctl restart cncnet"
echo "• Uninstall: $INSTALL_DIR/uninstall.sh"

echo -e "\n${YELLOW}=== Performance Monitoring ===${NC}"
echo "• CPU usage: top -p \$(pgrep cncnet-server)"
echo "• Network: sudo tcpdump -i any -n port 50000 or port 50001"
echo "• Metrics: curl -s localhost:9090/metrics | grep cncnet"
echo "• Connections: ss -tunap | grep ESTABLISHED | grep cncnet"

echo -e "\n${GREEN}Build and installation complete on Amazon Linux!${NC}"
echo -e "Instance type: t4g.medium (2 vCPU, 4GB RAM)"
echo -e "Architecture: ARM64/Graviton2"