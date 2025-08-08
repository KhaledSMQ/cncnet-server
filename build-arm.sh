#!/bin/bash
# Build and deployment script for CnCNet server on ARM64/AWS Graviton2

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

echo -e "${GREEN}=== CnCNet Server ARM Build Script ===${NC}"

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

# Install dependencies
echo -e "${GREEN}Installing build dependencies...${NC}"
sudo apt-get update
sudo apt-get install -y \
    build-essential \
    pkg-config \
    libssl-dev \
    lld \
    clang \
    libjemalloc-dev \
    curl \
    git

# Install Rust if not present
if ! command -v cargo &> /dev/null; then
    echo -e "${GREEN}Installing Rust...${NC}"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source $HOME/.cargo/env
fi

# Set ARM-specific build flags
if [[ "$GRAVITON" == true ]]; then
    export RUSTFLAGS="-C target-cpu=neoverse-n1 -C link-arg=-fuse-ld=lld -C target-feature=+crc,+aes,+sha2,+fp16,+rcpc,+dotprod,+crypto"
    echo -e "${GREEN}Using Graviton2-specific optimizations${NC}"
else
    export RUSTFLAGS="-C target-cpu=native -C link-arg=-fuse-ld=lld"
    echo -e "${GREEN}Using native CPU optimizations${NC}"
fi

# Additional optimizations for release builds
if [[ "$BUILD_TYPE" == "release" ]]; then
    export RUSTFLAGS="$RUSTFLAGS -C lto=fat -C opt-level=3 -C codegen-units=1"
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
    aarch64-linux-gnu-strip "$BINARY_PATH" 2>/dev/null || strip "$BINARY_PATH"
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
    sudo useradd -r -s /bin/false -d $INSTALL_DIR cncnet
fi

# Copy binary
echo -e "${GREEN}Installing binary...${NC}"
sudo cp "$BINARY_PATH" "$INSTALL_DIR/"
sudo chown cncnet:cncnet "$INSTALL_DIR/$PROJECT_NAME"
sudo chmod +x "$INSTALL_DIR/$PROJECT_NAME"

# Install systemd service
echo -e "${GREEN}Installing systemd service...${NC}"
sudo cp cncnet.service /etc/systemd/system/
sudo systemctl daemon-reload

# Apply kernel optimizations
echo -e "${GREEN}Applying kernel optimizations...${NC}"
sudo cp 99-cncnet-arm.conf /etc/sysctl.d/
sudo sysctl -p /etc/sysctl.d/99-cncnet-arm.conf

# Set up log rotation
echo -e "${GREEN}Setting up log rotation...${NC}"
cat << EOF | sudo tee /etc/logrotate.d/cncnet
$INSTALL_DIR/logs/*.log {
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

# Enable RPS (Receive Packet Steering) for better packet distribution
if [[ "$GRAVITON" == true ]]; then
    echo -e "${GREEN}Configuring RPS for network interfaces...${NC}"
    for interface in $(ls /sys/class/net/ | grep -v lo); do
        echo 3 | sudo tee /sys/class/net/$interface/queues/rx-*/rps_cpus > /dev/null 2>&1 || true
    done
fi

# Set IRQ affinity for network interrupts
echo -e "${GREEN}Optimizing IRQ affinity...${NC}"
sudo systemctl stop irqbalance 2>/dev/null || true
sudo systemctl disable irqbalance 2>/dev/null || true

# Performance governor
if [[ -f /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor ]]; then
    echo -e "${GREEN}Setting CPU governor to performance...${NC}"
    echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor > /dev/null 2>&1 || true
fi

# Create environment file
echo -e "${GREEN}Creating environment configuration...${NC}"
cat << EOF | sudo tee $INSTALL_DIR/.env
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

# Jemalloc tuning for ARM
LD_PRELOAD=/usr/lib/aarch64-linux-gnu/libjemalloc.so.2
MALLOC_CONF=background_thread:true,dirty_decay_ms:5000,muzzy_decay_ms:10000
EOF

sudo chown -R cncnet:cncnet $INSTALL_DIR

# Start service
echo -e "${GREEN}Starting $SERVICE_NAME service...${NC}"
sudo systemctl enable $SERVICE_NAME
sudo systemctl restart $SERVICE_NAME

# Wait for service to start
sleep 3

# Check service status
if sudo systemctl is-active --quiet $SERVICE_NAME; then
    echo -e "${GREEN}✓ Service started successfully!${NC}"

    # Show service status
    sudo systemctl status $SERVICE_NAME --no-pager | head -n 10

    # Test endpoints
    echo -e "\n${GREEN}Testing endpoints...${NC}"

    # Health check
    if curl -s -o /dev/null -w "%{http_code}" http://localhost:9090/health | grep -q "200"; then
        echo -e "${GREEN}✓ Health endpoint: OK${NC}"
    else
        echo -e "${RED}✗ Health endpoint: Failed${NC}"
    fi

    # Metrics
    if curl -s http://localhost:9090/metrics | grep -q "cncnet"; then
        echo -e "${GREEN}✓ Metrics endpoint: OK${NC}"
    else
        echo -e "${RED}✗ Metrics endpoint: Failed${NC}"
    fi

    # V2 Status
    if curl -s http://localhost:50000/status | grep -q "slots"; then
        echo -e "${GREEN}✓ V2 Status endpoint: OK${NC}"
    else
        echo -e "${RED}✗ V2 Status endpoint: Failed${NC}"
    fi

    echo -e "\n${GREEN}=== Installation Complete ===${NC}"
    echo -e "Server is running on ports 50000 (v2) and 50001 (v3)"
    echo -e "Metrics available at: http://localhost:9090/metrics"
    echo -e "Logs: sudo journalctl -u $SERVICE_NAME -f"
else
    echo -e "${RED}✗ Service failed to start${NC}"
    sudo journalctl -u $SERVICE_NAME -n 50 --no-pager
    exit 1
fi

# Performance tips
echo -e "\n${YELLOW}=== Performance Tips for t4g.medium ===${NC}"
echo "1. Monitor CPU: watch -n 1 'top -b -n 1 | head -20'"
echo "2. Monitor network: sudo iftop -i eth0"
echo "3. Monitor metrics: curl -s localhost:9090/metrics | grep cncnet"
echo "4. View logs: sudo journalctl -u cncnet -f"
echo "5. Check connections: ss -tunap | grep -E '5000[01]'"

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

echo -e "\n${GREEN}Build and installation complete!${NC}"