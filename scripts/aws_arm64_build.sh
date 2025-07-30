#!/bin/bash

# CnCNet Server AWS ARM64 Build and Deployment Script
# Author: Khaled Sameer <khaled.smq@hotmail.com>
# Repository: https://github.com/khaledsmq/cncnet-server
# Description: Complete setup script for fresh AWS Linux ARM64 instance

set -euo pipefail

# Color codes for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration variables
REPO_URL="https://github.com/khaledsmq/cncnet-server"
PROJECT_NAME="cncnet-server"
SERVICE_USER="cncnet"
INSTALL_DIR="/opt/cncnet-server"
LOG_DIR="/var/log/cncnet-server"
CONFIG_DIR="/etc/cncnet-server"
SYSTEMD_SERVICE="cncnet-server.service"

# Rust configuration
RUST_VERSION="stable"
CARGO_TARGET="aarch64-unknown-linux-gnu"

# Network ports
V3_PORT="${V3_PORT:-50001}"
V2_PORT="${V2_PORT:-50000}"
P2P_PORT1="${P2P_PORT1:-8054}"
P2P_PORT2="${P2P_PORT2:-3478}"


SERVER_NAME="${SERVER_NAME:-CnCNet Server}"
MASTER_URL="${MASTER_URL:-http://cncnet.org/master-announce}"
NO_MASTER="${NO_MASTER:-false}"
MAINT_PASSWORD="${MAINT_PASSWORD:-$(openssl rand -base64 12)}"

# Performance tuning defaults
MAX_CLIENTS="${MAX_CLIENTS:-1000}"
IP_LIMIT_V3="${IP_LIMIT_V3:-8}"
IP_LIMIT_V2="${IP_LIMIT_V2:-4}"

# Logging functions - ALWAYS output to stderr
log_info() {
    echo -e "${BLUE}[INFO]${NC} $1" >&2
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1" >&2
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1" >&2
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1" >&2
}

# Check if running as root
check_root() {
    if [[ $EUID -ne 0 ]]; then
        log_error "This script must be run as root"
        exit 1
    fi
}

# Detect Linux distribution
detect_distro() {
    if [[ -f /etc/os-release ]]; then
        . /etc/os-release
        OS=$ID
        VER=$VERSION_ID
    else
        log_error "Cannot detect Linux distribution"
        exit 1
    fi
    log_info "Detected: $OS $VER"
}

# Update system packages
update_system() {
    log_info "Updating system packages..."

    case $OS in
        ubuntu|debian)
            apt-get update -y
            apt-get upgrade -y
            apt-get install -y \
                build-essential \
                git \
                wget \
                pkg-config \
                libssl-dev \
                htop \
                iotop \
                net-tools \
                iptables-persistent \
                fail2ban \
                rsyslog \
                logrotate \
                chrony \
                conntrack
            ;;
        amzn|centos|rhel|fedora)
            yum update -y
            yum groupinstall -y "Development Tools"
            yum install -y \
                git \
                wget \
                openssl-devel \
                htop \
                iotop \
                net-tools \
                iptables-services \
                fail2ban \
                rsyslog \
                logrotate \
                chrony \
                conntrack-tools
            ;;
        *)
            log_error "Unsupported distribution: $OS"
            exit 1
            ;;
    esac

    log_success "System packages updated"
}

# Install Rust for ARM64
install_rust() {
    log_info "Installing Rust for ARM64..."

    # Install rustup
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain $RUST_VERSION

    # Source cargo env
    source "$HOME/.cargo/env"

    # Add to profile
    echo 'source $HOME/.cargo/env' >> ~/.bashrc

    # Verify installation
    rustc --version >&2
    cargo --version >&2

    # Add ARM64 target (should be default on ARM64, but ensure it's there)
    rustup target add $CARGO_TARGET

    log_success "Rust installed successfully"
}

# Load necessary kernel modules
load_kernel_modules() {
    log_info "Loading kernel modules for connection tracking..."

    # Load netfilter modules
    local modules=(
        "nf_conntrack"
        "nf_conntrack_ipv4"
        "nf_conntrack_ipv6"
        "nf_conntrack_netlink"
        "xt_conntrack"
    )

    for module in "${modules[@]}"; do
        if modprobe $module 2>/dev/null; then
            log_info "Loaded module: $module"
        else
            log_warn "Could not load module: $module (may not exist on this kernel)"
        fi
    done

    # Make modules persistent
    if [[ -d /etc/modules-load.d ]]; then
        echo "# Netfilter connection tracking modules" > /etc/modules-load.d/netfilter.conf
        for module in "${modules[@]}"; do
            echo "$module" >> /etc/modules-load.d/netfilter.conf
        done
    fi

    log_success "Kernel modules configured"
}

# Configure system limits for high performance
configure_system_limits() {
    log_info "Configuring system limits for high performance..."

    # Backup original files
    cp /etc/security/limits.conf /etc/security/limits.conf.backup
    cp /etc/sysctl.conf /etc/sysctl.conf.backup

    # Configure limits.conf
    cat >> /etc/security/limits.conf << EOF

# CnCNet Server Limits
$SERVICE_USER soft nofile 1048576
$SERVICE_USER hard nofile 1048576
$SERVICE_USER soft nproc 65536
$SERVICE_USER hard nproc 65536
$SERVICE_USER soft memlock unlimited
$SERVICE_USER hard memlock unlimited
EOF

    # Configure sysctl for network performance
    cat >> /etc/sysctl.conf << EOF

# CnCNet Server Network Optimizations
# Increase system IP port limits
net.ipv4.ip_local_port_range = 1024 65535

# Increase TCP buffer sizes for better throughput
net.core.rmem_max = 134217728
net.core.wmem_max = 134217728
net.ipv4.tcp_rmem = 4096 87380 134217728
net.ipv4.tcp_wmem = 4096 65536 134217728

# Increase UDP buffer sizes
net.core.netdev_max_backlog = 30000
net.ipv4.udp_rmem_min = 8192
net.ipv4.udp_wmem_min = 8192

# Enable TCP Fast Open
net.ipv4.tcp_fastopen = 3

# Security hardening
net.ipv4.tcp_syncookies = 1
net.ipv4.tcp_rfc1337 = 1
net.ipv4.conf.all.rp_filter = 1
net.ipv4.conf.default.rp_filter = 1

# Increase socket listen backlog
net.core.somaxconn = 65535
net.ipv4.tcp_max_syn_backlog = 65535

# Enable BBR congestion control if available
net.core.default_qdisc = fq
net.ipv4.tcp_congestion_control = bbr

# Optimize for low latency
net.ipv4.tcp_low_latency = 1
net.ipv4.tcp_sack = 1
net.ipv4.tcp_timestamps = 1
EOF

    # Add connection tracking settings only if the module is loaded
    if lsmod | grep -q nf_conntrack; then
        cat >> /etc/sysctl.conf << EOF

# Connection tracking for NAT (if available)
net.netfilter.nf_conntrack_max = 1048576
net.nf_conntrack_max = 1048576
net.netfilter.nf_conntrack_udp_timeout = 30
net.netfilter.nf_conntrack_udp_timeout_stream = 60
EOF
    else
        log_warn "Connection tracking modules not available - skipping conntrack settings"
    fi

    # Apply sysctl settings, ignoring errors for missing parameters
    sysctl -p 2>&1 | grep -v "No such file or directory" || true

    log_success "System limits configured"
}

# Configure firewall rules
configure_firewall() {
    log_info "Configuring firewall rules..."

    # Enable firewall
    case $OS in
        ubuntu|debian)
            # Install ufw if not present
            apt-get install -y ufw

            # Configure UFW
            ufw --force reset
            ufw default deny incoming
            ufw default allow outgoing

            # Allow SSH (adjust port if needed)
            ufw allow 22/tcp comment "SSH"

            # Allow CnCNet ports
            ufw allow $V3_PORT/udp comment "CnCNet V3"
            ufw allow $V2_PORT/udp comment "CnCNet V2 UDP"
            ufw allow $V2_PORT/tcp comment "CnCNet V2 HTTP"
            ufw allow $P2P_PORT1/udp comment "CnCNet P2P"
            ufw allow $P2P_PORT2/udp comment "CnCNet STUN"

            # Enable UFW
            ufw --force enable
            ;;

        amzn|centos|rhel|fedora)
            # Configure iptables
            systemctl enable iptables 2>/dev/null || true
            systemctl start iptables 2>/dev/null || true

            # Save current rules
            if command -v iptables-save &> /dev/null; then
                iptables-save > /etc/sysconfig/iptables.backup 2>/dev/null || true
            fi

            # Clear existing rules
            iptables -F
            iptables -X

            # Default policies
            iptables -P INPUT DROP
            iptables -P FORWARD DROP
            iptables -P OUTPUT ACCEPT

            # Allow loopback
            iptables -A INPUT -i lo -j ACCEPT

            # Allow established connections
            iptables -A INPUT -m state --state ESTABLISHED,RELATED -j ACCEPT

            # Allow SSH
            iptables -A INPUT -p tcp --dport 22 -j ACCEPT

            # Allow CnCNet ports
            iptables -A INPUT -p udp --dport $V3_PORT -j ACCEPT
            iptables -A INPUT -p udp --dport $V2_PORT -j ACCEPT
            iptables -A INPUT -p tcp --dport $V2_PORT -j ACCEPT
            iptables -A INPUT -p udp --dport $P2P_PORT1 -j ACCEPT
            iptables -A INPUT -p udp --dport $P2P_PORT2 -j ACCEPT

            # Save rules
            if command -v service &> /dev/null; then
                service iptables save 2>/dev/null || true
            elif [[ -f /etc/sysconfig/iptables ]]; then
                iptables-save > /etc/sysconfig/iptables
            fi
            ;;
    esac

    log_success "Firewall configured"
}

# Create service user
create_service_user() {
    log_info "Creating service user..."

    if id "$SERVICE_USER" &>/dev/null; then
        log_warn "User $SERVICE_USER already exists"
    else
        useradd -r -s /sbin/nologin -d $INSTALL_DIR -c "CnCNet Server" $SERVICE_USER
        log_success "Service user created"
    fi
}

# Clone and build the project
build_project() {
    log_info "Cloning and building the project..."

    # Create temporary build directory
    local BUILD_DIR=$(mktemp -d)
    cd $BUILD_DIR

    # Clone repository
    git clone $REPO_URL >&2
    cd $PROJECT_NAME

    # Build with optimizations for ARM64
    log_info "Building release binary for ARM64..."

    # Set build flags for maximum optimization
    export RUSTFLAGS="-C target-cpu=native -C opt-level=3 -C lto=fat -C codegen-units=1"

    # Build release binary
    cargo build --release --target $CARGO_TARGET >&2

    # Strip debug symbols (check if strip tool exists)
    if command -v aarch64-linux-gnu-strip &> /dev/null; then
        aarch64-linux-gnu-strip target/$CARGO_TARGET/release/$PROJECT_NAME >&2
    elif command -v strip &> /dev/null; then
        strip target/$CARGO_TARGET/release/$PROJECT_NAME >&2
    fi

    # Verify binary
    file target/$CARGO_TARGET/release/$PROJECT_NAME >&2

    log_success "Project built successfully"

    # Return build directory - ONLY output the path, nothing else
    echo $BUILD_DIR
}

# Install the application
install_application() {
    local build_dir=$1

    log_info "Installing application..."

    # Create directories
    mkdir -p $INSTALL_DIR
    mkdir -p $LOG_DIR
    mkdir -p $CONFIG_DIR

    # Copy binary
    cp $build_dir/$PROJECT_NAME/target/$CARGO_TARGET/release/$PROJECT_NAME $INSTALL_DIR/
    chmod +x $INSTALL_DIR/$PROJECT_NAME

    # Set ownership
    chown -R $SERVICE_USER:$SERVICE_USER $INSTALL_DIR
    chown -R $SERVICE_USER:$SERVICE_USER $LOG_DIR

    # Create default configuration
    cat > $CONFIG_DIR/server.conf << EOF
# CnCNet Server Configuration
# Generated on $(date)

# Server identification
SERVER_NAME="${SERVER_NAME:-CnCNet Server}"

# Network ports
V3_PORT=$V3_PORT
V2_PORT=$V2_PORT

# Performance settings
MAX_CLIENTS=$MAX_CLIENTS
IP_LIMIT_V3=$IP_LIMIT_V3
IP_LIMIT_V2=$IP_LIMIT_V2

# Master server
MASTER_URL="${MASTER_URL:-http://cncnet.org/master-announce}"
NO_MASTER="${NO_MASTER:-false}"

# Security
MAINT_PASSWORD="${MAINT_PASSWORD:-$(openssl rand -base64 12)}"

# Logging
LOG_LEVEL="${LOG_LEVEL:-info}"

EOF

    chmod 600 $CONFIG_DIR/server.conf

    # Clean up build directory
    rm -rf $build_dir

    log_success "Application installed"
}

# Create systemd service
create_systemd_service() {
    log_info "Creating systemd service..."

    cat > /etc/systemd/system/$SYSTEMD_SERVICE << EOF
[Unit]
Description=CnCNet High-Performance Game Server
Documentation=https://github.com/khaledsmq/cncnet-server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$SERVICE_USER
Group=$SERVICE_USER
WorkingDirectory=$INSTALL_DIR

# Security hardening
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=$LOG_DIR
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictRealtime=true
RestrictNamespaces=true
RestrictSUIDSGID=true
MemoryDenyWriteExecute=true
LockPersonality=true

# Resource limits
LimitNOFILE=1048576
LimitNPROC=65536
TasksMax=65536

# Restart policy
Restart=always
RestartSec=5
StartLimitInterval=300
StartLimitBurst=5

# Environment
Environment="RUST_LOG=$LOG_LEVEL"
Environment="RUST_BACKTRACE=1"

# Command with all parameters
ExecStart=$INSTALL_DIR/$PROJECT_NAME \\
    --port $V3_PORT \\
    --portv2 $V2_PORT \\
    --name "$SERVER_NAME" \\
    --maxclients $MAX_CLIENTS \\
    --iplimit $IP_LIMIT_V3 \\
    --iplimitv2 $IP_LIMIT_V2 \\
    --master "$MASTER_URL" \\
    --maintpw "$MAINT_PASSWORD" \\
    \$([ "$NO_MASTER" = "true" ] && echo "--nomaster")

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=cncnet-server

[Install]
WantedBy=multi-user.target
EOF

    # Reload systemd
    systemctl daemon-reload

    log_success "Systemd service created"
}

# Configure logging
configure_logging() {
    log_info "Configuring logging..."

    # Create rsyslog configuration
    cat > /etc/rsyslog.d/50-cncnet.conf << 'EOF'
# CnCNet Server Logging Configuration
if $programname == 'cncnet-server' then {
    # Log to dedicated file
    action(type="omfile" file="/var/log/cncnet-server/server.log")

    # Log errors separately
    if $syslogseverity <= 3 then {
        action(type="omfile" file="/var/log/cncnet-server/error.log")
    }

    # Stop processing after logging
    stop
}
EOF

    # Create logrotate configuration
    cat > /etc/logrotate.d/cncnet-server << EOF
$LOG_DIR/*.log {
    daily
    rotate 14
    compress
    delaycompress
    missingok
    notifempty
    create 0640 $SERVICE_USER $SERVICE_USER
    sharedscripts
    postrotate
        systemctl reload rsyslog > /dev/null 2>&1 || true
    endscript
}
EOF

    # Restart rsyslog
    systemctl restart rsyslog

    log_success "Logging configured"
}

# Configure monitoring
configure_monitoring() {
    log_info "Configuring monitoring..."

    # Create monitoring script
    cat > $INSTALL_DIR/health_check.sh << 'EOF'
#!/bin/bash
# CnCNet Server Health Check

# Check if service is running
if ! systemctl is-active --quiet cncnet-server; then
    echo "CRITICAL: CnCNet Server is not running"
    exit 2
fi

# Check UDP ports
for port in 50001 50000 8054 3478; do
    if ! ss -uln | grep -q ":$port "; then
        echo "WARNING: UDP port $port is not listening"
        exit 1
    fi
done

# Check TCP port (V2 HTTP)
if ! ss -tln | grep -q ":50000 "; then
    echo "WARNING: TCP port 50000 is not listening"
    exit 1
fi

echo "OK: CnCNet Server is healthy"
exit 0
EOF

    chmod +x $INSTALL_DIR/health_check.sh

    # Create cron job for monitoring
    cat > /etc/cron.d/cncnet-monitor << EOF
# CnCNet Server Monitoring
*/5 * * * * root $INSTALL_DIR/health_check.sh || systemctl restart cncnet-server
EOF

    log_success "Monitoring configured"
}

# Configure fail2ban for DDoS protection
configure_fail2ban() {
    log_info "Configuring fail2ban..."

    # Create fail2ban filter
    cat > /etc/fail2ban/filter.d/cncnet.conf << 'EOF'
[Definition]
failregex = Rate limit exceeded for IP: <HOST>
            Rate limit exceeded for <HOST>
ignoreregex =
EOF

    # Create fail2ban jail
    cat > /etc/fail2ban/jail.d/cncnet.conf << EOF
[cncnet]
enabled = true
filter = cncnet
action = iptables-multiport[name=cncnet, port="$V3_PORT,$V2_PORT,$P2P_PORT1,$P2P_PORT2", protocol=udp]
logpath = /var/log/cncnet-server/server.log
maxretry = 5
findtime = 60
bantime = 3600
EOF

    # Restart fail2ban
    systemctl restart fail2ban 2>/dev/null || true
    systemctl enable fail2ban 2>/dev/null || true

    log_success "Fail2ban configured"
}

# Performance tuning specific to ARM64
tune_arm64_performance() {
    log_info "Tuning ARM64 performance..."

    # Set CPU governor to performance
    if [[ -f /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor ]]; then
        for cpu in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
            echo performance > $cpu 2>/dev/null || true
        done
    fi

    # Disable CPU frequency scaling service if exists
    systemctl stop cpufrequtils 2>/dev/null || true
    systemctl disable cpufrequtils 2>/dev/null || true

    # Set IRQ affinity for network interfaces
    # This spreads network interrupts across all CPU cores
    for irq in $(grep -E 'eth|ens|enp' /proc/interrupts | awk '{print $1}' | sed 's/://'); do
        echo ff > /proc/irq/$irq/smp_affinity 2>/dev/null || true
    done

    log_success "ARM64 performance tuned"
}

# Start and enable services
start_services() {
    log_info "Starting services..."

    # Enable and start the service
    systemctl enable $SYSTEMD_SERVICE
    systemctl start $SYSTEMD_SERVICE

    # Wait for service to start
    sleep 3

    # Check status
    if systemctl is-active --quiet $SYSTEMD_SERVICE; then
        log_success "CnCNet Server started successfully"
        systemctl status $SYSTEMD_SERVICE --no-pager
    else
        log_error "Failed to start CnCNet Server"
        journalctl -u $SYSTEMD_SERVICE --no-pager -n 50
        exit 1
    fi
}

# Display final information
display_info() {
    local maint_pw=$(grep MAINT_PASSWORD $CONFIG_DIR/server.conf | cut -d'=' -f2 | cut -d'"' -f2)

    echo >&2
    echo "=========================================" >&2
    echo "   CnCNet Server Installation Complete" >&2
    echo "=========================================" >&2
    echo >&2
    echo "Server Status: $(systemctl is-active $SYSTEMD_SERVICE)" >&2
    echo "Configuration: $CONFIG_DIR/server.conf" >&2
    echo "Logs: $LOG_DIR/" >&2
    echo >&2
    echo "Network Ports:" >&2
    echo "  - Tunnel V3: UDP $V3_PORT" >&2
    echo "  - Tunnel V2: UDP/TCP $V2_PORT" >&2
    echo "  - P2P NAT:   UDP $P2P_PORT1, $P2P_PORT2" >&2
    echo >&2
    echo "Maintenance Password: $maint_pw" >&2
    echo >&2
    echo "Useful Commands:" >&2
    echo "  - View logs:     journalctl -u $SYSTEMD_SERVICE -f" >&2
    echo "  - Check status:  systemctl status $SYSTEMD_SERVICE" >&2
    echo "  - Restart:       systemctl restart $SYSTEMD_SERVICE" >&2
    echo "  - Health check:  $INSTALL_DIR/health_check.sh" >&2
    echo >&2
    echo "=========================================" >&2
}

# Main installation flow
main() {
    log_info "Starting CnCNet Server installation for AWS ARM64"

    # Pre-flight checks
    check_root
    detect_distro

    # System preparation
    update_system
#    load_kernel_modules
    install_rust
#    configure_system_limits
#    configure_firewall

    # User and directories
    create_service_user

    # Build and install
    BUILD_DIR=$(build_project)
    install_application $BUILD_DIR

    # Service configuration
    create_systemd_service
#    configure_logging
#    configure_monitoring
#    configure_fail2ban
#    tune_arm64_performance

    # Start services
    start_services

    # Display summary
    display_info

    log_success "Installation completed successfully!"
}

# Run main function
main "$@"