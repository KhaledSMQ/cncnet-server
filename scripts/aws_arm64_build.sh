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

# Server configuration
SERVER_NAME="${SERVER_NAME:-CnCNet Server}"
MASTER_URL="${MASTER_URL:-http://cncnet.org/master-announce}"
NO_MASTER="${NO_MASTER:-false}"
MAINT_PASSWORD="${MAINT_PASSWORD:-$(openssl rand -base64 12)}"

# Performance tuning defaults
MAX_CLIENTS="${MAX_CLIENTS:-200}"
IP_LIMIT_V3="${IP_LIMIT_V3:-8}"
IP_LIMIT_V2="${IP_LIMIT_V2:-4}"

# Logging configuration
LOG_LEVEL="${LOG_LEVEL:-info}"

# Installation mode
FORCE_REINSTALL="${FORCE_REINSTALL:-false}"

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

# Function to escape strings for systemd
escape_systemd_string() {
    local input="$1"
    # Escape backslashes first, then double quotes
    echo "$input" | sed 's/\\/\\\\/g' | sed 's/"/\\"/g'
}

# Check if running as root
check_root() {
    if [[ $EUID -ne 0 ]]; then
        log_error "This script must be run as root"
        exit 1
    fi
}

# Check for existing installation
check_existing_installation() {
    log_info "Checking for existing installation..."

    local has_existing=false

    if [[ -f /etc/systemd/system/$SYSTEMD_SERVICE ]]; then
        log_warn "Found existing systemd service: $SYSTEMD_SERVICE"
        has_existing=true
    fi

    if [[ -d $INSTALL_DIR && -f $INSTALL_DIR/$PROJECT_NAME ]]; then
        log_warn "Found existing installation in: $INSTALL_DIR"
        has_existing=true
    fi

    if [[ "$has_existing" == "true" && "$FORCE_REINSTALL" != "true" ]]; then
        log_error "Existing installation detected. To reinstall, run with FORCE_REINSTALL=true"
        log_info "Example: FORCE_REINSTALL=true $0"
        exit 1
    fi

    if [[ "$has_existing" == "true" && "$FORCE_REINSTALL" == "true" ]]; then
        log_warn "Force reinstall requested. Cleaning up existing installation..."

        # Stop and disable service if running
        if systemctl is-active --quiet $SYSTEMD_SERVICE; then
            systemctl stop $SYSTEMD_SERVICE
        fi
        if systemctl is-enabled --quiet $SYSTEMD_SERVICE; then
            systemctl disable $SYSTEMD_SERVICE
        fi

        # Remove service file
        rm -f /etc/systemd/system/$SYSTEMD_SERVICE
        systemctl daemon-reload
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
                conntrack \
                cron
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
                conntrack-tools \
                cronie \
                crontabs
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
    log_info "Checking Rust installation..."

    # Check if Rust is already installed
    if command -v rustc &> /dev/null && command -v cargo &> /dev/null; then
        local installed_version=$(rustc --version | awk '{print $2}')
        log_info "Rust is already installed (version: $installed_version)"

        # Source cargo env just in case
        if [[ -f "$HOME/.cargo/env" ]]; then
            source "$HOME/.cargo/env"
        fi

        # Ensure ARM64 target is added
        rustup target add $CARGO_TARGET 2>/dev/null || true

        log_success "Rust verified"
        return
    fi

    log_info "Installing Rust for ARM64..."

    # Install rustup
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain $RUST_VERSION

    # Source cargo env
    source "$HOME/.cargo/env"

    # Add to profile if not already there
    if ! grep -q 'source $HOME/.cargo/env' ~/.bashrc; then
        echo 'source $HOME/.cargo/env' >> ~/.bashrc
    fi

    # Verify installation
    rustc --version >&2
    cargo --version >&2

    # Add ARM64 target
    rustup target add $CARGO_TARGET

    log_success "Rust installed successfully"
}

# Load necessary kernel modules (AWS-safe version)
load_kernel_modules() {
    log_info "Loading kernel modules for connection tracking..."

    # Only try to load modules that exist on Amazon Linux
    local modules=(
        "nf_conntrack"
    )

    for module in "${modules[@]}"; do
        if lsmod | grep -q "^$module"; then
            log_info "Module already loaded: $module"
        elif modprobe -n $module 2>/dev/null; then
            if modprobe $module 2>/dev/null; then
                log_info "Loaded module: $module"
            else
                log_warn "Could not load module: $module"
            fi
        else
            log_warn "Module not available: $module"
        fi
    done

    # Make modules persistent if the directory exists
    if [[ -d /etc/modules-load.d ]]; then
        # Check if already configured
        if [[ ! -f /etc/modules-load.d/netfilter.conf ]]; then
            echo "# Netfilter connection tracking modules" > /etc/modules-load.d/netfilter.conf
            for module in "${modules[@]}"; do
                if lsmod | grep -q "^$module"; then
                    echo "$module" >> /etc/modules-load.d/netfilter.conf
                fi
            done
        else
            log_info "Kernel modules already configured"
        fi
    fi

    log_success "Kernel modules configured"
}

# Configure system limits for high performance (AWS-safe)
configure_system_limits() {
    log_info "Configuring system limits for high performance..."

    # Backup original files if not already backed up
    if [[ ! -f /etc/security/limits.conf.backup ]]; then
        cp /etc/security/limits.conf /etc/security/limits.conf.backup
    fi
    if [[ ! -f /etc/sysctl.conf.backup ]]; then
        cp /etc/sysctl.conf /etc/sysctl.conf.backup
    fi

    # Check if limits are already configured
    if ! grep -q "# CnCNet Server Limits" /etc/security/limits.conf; then
        cat >> /etc/security/limits.conf << EOF

# CnCNet Server Limits
$SERVICE_USER soft nofile 1048576
$SERVICE_USER hard nofile 1048576
$SERVICE_USER soft nproc 65536
$SERVICE_USER hard nproc 65536
$SERVICE_USER soft memlock unlimited
$SERVICE_USER hard memlock unlimited
EOF
    else
        log_info "System limits already configured"
    fi

    # Check if sysctl settings are already configured
    if ! grep -q "# CnCNet Server Network Optimizations" /etc/sysctl.conf; then
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

# Enable TCP Fast Open
net.ipv4.tcp_fastopen = 3

# Increase socket listen backlog
net.core.somaxconn = 65535
net.ipv4.tcp_max_syn_backlog = 65535

# Optimize for low latency
net.ipv4.tcp_low_latency = 1
net.ipv4.tcp_sack = 1
net.ipv4.tcp_timestamps = 1
EOF

        # Only add BBR if it's available
        if modinfo tcp_bbr &>/dev/null; then
            cat >> /etc/sysctl.conf << EOF

# Enable BBR congestion control (if available)
net.core.default_qdisc = fq
net.ipv4.tcp_congestion_control = bbr
EOF
        fi

        # Add connection tracking settings only if the module is loaded
        if lsmod | grep -q nf_conntrack; then
            cat >> /etc/sysctl.conf << EOF

# Connection tracking for NAT (if available)
net.netfilter.nf_conntrack_max = 1048576
net.nf_conntrack_max = 1048576
net.netfilter.nf_conntrack_udp_timeout = 30
net.netfilter.nf_conntrack_udp_timeout_stream = 60
EOF
        fi
    else
        log_info "Sysctl settings already configured"
    fi

    # Apply sysctl settings, ignoring errors for missing parameters
    sysctl -p 2>&1 | grep -v "No such file or directory" || true

    log_success "System limits configured"
}

# Configure firewall rules
configure_firewall() {
    log_info "Configuring firewall rules..."

    # Check if rules already exist
    local rules_exist=false

    case $OS in
        ubuntu|debian)
            if command -v ufw &> /dev/null && ufw status | grep -q "$V3_PORT/udp"; then
                rules_exist=true
            fi
            ;;
        amzn|centos|rhel|fedora)
            if iptables -L INPUT -n | grep -q "CnCNet"; then
                rules_exist=true
            fi
            ;;
    esac

    if [[ "$rules_exist" == "true" ]]; then
        log_info "Firewall rules already configured"
        return
    fi

    # For Amazon Linux, we'll use iptables directly
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
            # For Amazon Linux, we'll add rules without changing default policies
            # This prevents losing SSH access

            # Check if iptables service exists
            if systemctl list-unit-files | grep -q iptables.service; then
                systemctl enable iptables 2>/dev/null || true
                systemctl start iptables 2>/dev/null || true
            fi

            # Save current rules
            if command -v iptables-save &> /dev/null; then
                iptables-save > /tmp/iptables.backup 2>/dev/null || true
            fi

            # Add CnCNet rules without modifying existing AWS security group rules
            # Allow CnCNet ports (INPUT)
            iptables -I INPUT 1 -p udp --dport $V3_PORT -j ACCEPT -m comment --comment "CnCNet V3"
            iptables -I INPUT 1 -p udp --dport $V2_PORT -j ACCEPT -m comment --comment "CnCNet V2 UDP"
            iptables -I INPUT 1 -p tcp --dport $V2_PORT -j ACCEPT -m comment --comment "CnCNet V2 HTTP"
            iptables -I INPUT 1 -p udp --dport $P2P_PORT1 -j ACCEPT -m comment --comment "CnCNet P2P"
            iptables -I INPUT 1 -p udp --dport $P2P_PORT2 -j ACCEPT -m comment --comment "CnCNet STUN"

            # Save rules based on the system
            if [[ -f /etc/sysconfig/iptables ]]; then
                iptables-save > /etc/sysconfig/iptables
            elif command -v netfilter-persistent &> /dev/null; then
                netfilter-persistent save
            fi

            log_warn "Remember to also configure AWS Security Group to allow ports: $V3_PORT/udp, $V2_PORT/tcp+udp, $P2P_PORT1/udp, $P2P_PORT2/udp"
            ;;
    esac

    log_success "Firewall configured"
}

# Create service user
create_service_user() {
    log_info "Creating service user..."

    if id "$SERVICE_USER" &>/dev/null; then
        log_info "User $SERVICE_USER already exists"
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

    # Create or update configuration
    if [[ -f $CONFIG_DIR/server.conf ]]; then
        log_info "Configuration file already exists, preserving existing settings"
        # Backup existing config
        cp $CONFIG_DIR/server.conf $CONFIG_DIR/server.conf.$(date +%Y%m%d_%H%M%S)
    else
        # Don't escape values in the config file - they should be stored as-is
        cat > $CONFIG_DIR/server.conf << EOF
# CnCNet Server Configuration
# Generated on $(date)

# Server identification
SERVER_NAME="${SERVER_NAME}"

# Network ports
V3_PORT=$V3_PORT
V2_PORT=$V2_PORT

# Performance settings
MAX_CLIENTS=$MAX_CLIENTS
IP_LIMIT_V3=$IP_LIMIT_V3
IP_LIMIT_V2=$IP_LIMIT_V2

# Master server
MASTER_URL="${MASTER_URL}"
NO_MASTER="${NO_MASTER}"

# Security
MAINT_PASSWORD="${MAINT_PASSWORD}"

# Logging
LOG_LEVEL="${LOG_LEVEL}"

EOF
      chown -R root:$SERVICE_USER $CONFIG_DIR
      chmod 750 $CONFIG_DIR
    fi

    # Clean up build directory
    rm -rf $build_dir

    log_success "Application installed"
}

# Create systemd service
create_systemd_service() {
    log_info "Creating systemd service..."

    # Check if service already exists
    if [[ -f /etc/systemd/system/$SYSTEMD_SERVICE ]]; then
        log_info "Systemd service already exists, updating..."
        systemctl stop $SYSTEMD_SERVICE 2>/dev/null || true
    fi

    # Escape special characters for systemd
    local escaped_server_name=$(escape_systemd_string "$SERVER_NAME")
    local escaped_master_url=$(escape_systemd_string "$MASTER_URL")
    local escaped_maint_password=$(escape_systemd_string "$MAINT_PASSWORD")

    # Create the service file with proper quoting
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
Environment="RUST_LOG=${LOG_LEVEL}"
Environment="RUST_BACKTRACE=1"

# Command with properly quoted parameters
ExecStart=$INSTALL_DIR/$PROJECT_NAME \\
    --port "$V3_PORT" \\
    --portv2 "$V2_PORT" \\
    --name "${escaped_server_name}" \\
    --maxclients "$MAX_CLIENTS" \\
    --iplimit "$IP_LIMIT_V3" \\
    --iplimitv2 "$IP_LIMIT_V2" \\
    --master "${escaped_master_url}" \\
    --maintpw "${escaped_maint_password}"$([ "$NO_MASTER" = "true" ] && echo " \\\\\n    --nomaster" || echo "")

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=cncnet-server

[Install]
WantedBy=multi-user.target
EOF

    # Alternative approach: Create a wrapper script for even better handling
    cat > $INSTALL_DIR/start-server.sh << 'EOF'
#!/bin/bash
# CnCNet Server startup wrapper
# This script ensures proper argument handling for special characters

# Enable debugging if needed
if [[ "${DEBUG_WRAPPER}" == "true" ]]; then
    set -x
fi

# Source configuration
if [[ -f /etc/cncnet-server/server.conf ]]; then
    source /etc/cncnet-server/server.conf
else
    echo "ERROR: Configuration file not found at /etc/cncnet-server/server.conf" >&2
    exit 1
fi

# Validate required variables
if [[ -z "$V3_PORT" ]] || [[ -z "$V2_PORT" ]] || [[ -z "$SERVER_NAME" ]]; then
    echo "ERROR: Required configuration variables are missing" >&2
    echo "V3_PORT=$V3_PORT" >&2
    echo "V2_PORT=$V2_PORT" >&2
    echo "SERVER_NAME=$SERVER_NAME" >&2
    exit 1
fi

# Build command array
CMD=(/opt/cncnet-server/cncnet-server)
CMD+=(--port "$V3_PORT")
CMD+=(--portv2 "$V2_PORT")
CMD+=(--name "$SERVER_NAME")
CMD+=(--maxclients "$MAX_CLIENTS")
CMD+=(--iplimit "$IP_LIMIT_V3")
CMD+=(--iplimitv2 "$IP_LIMIT_V2")
CMD+=(--master "$MASTER_URL")
CMD+=(--maintpw "$MAINT_PASSWORD")

# Add optional parameters
if [[ "$NO_MASTER" == "true" ]]; then
    CMD+=(--nomaster)
fi

# Log the command being executed (useful for debugging)
if [[ "${DEBUG_WRAPPER}" == "true" ]] || [[ "${RUST_LOG}" == "debug" ]]; then
    echo "Executing command: ${CMD[@]}" >&2
fi

# Execute with proper argument handling
exec "${CMD[@]}"
EOF

    chmod +x $INSTALL_DIR/start-server.sh
    chown $SERVICE_USER:$SERVICE_USER $INSTALL_DIR/start-server.sh

    # Update systemd service to use wrapper script
    cat > /etc/systemd/system/${SYSTEMD_SERVICE}.new << EOF
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
Environment="RUST_LOG=${LOG_LEVEL}"
Environment="RUST_BACKTRACE=1"
Environment="DEBUG_WRAPPER=false"

# Use wrapper script for better argument handling
ExecStart=$INSTALL_DIR/start-server.sh

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=cncnet-server

[Install]
WantedBy=multi-user.target
EOF

    # Move new service file into place
    mv /etc/systemd/system/${SYSTEMD_SERVICE}.new /etc/systemd/system/$SYSTEMD_SERVICE

    # Reload systemd
    systemctl daemon-reload

    log_success "Systemd service created"
}

# Configure logging
configure_logging() {
    log_info "Configuring logging..."

    # Ensure rsyslog.d directory exists
    if [[ ! -d /etc/rsyslog.d ]]; then
        mkdir -p /etc/rsyslog.d
    fi

    # Check if rsyslog configuration already exists
    if [[ ! -f /etc/rsyslog.d/50-cncnet.conf ]]; then
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
    else
        log_info "Rsyslog configuration already exists"
    fi

    # Ensure logrotate.d directory exists
    if [[ ! -d /etc/logrotate.d ]]; then
        mkdir -p /etc/logrotate.d
    fi

    # Check if logrotate configuration already exists
    if [[ ! -f /etc/logrotate.d/cncnet-server ]]; then
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
    else
        log_info "Logrotate configuration already exists"
    fi

    # Restart rsyslog if it exists
    if systemctl list-unit-files | grep -q "rsyslog.service"; then
        systemctl restart rsyslog 2>/dev/null || true
    fi

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
    # First check if cron.d directory exists, create if needed
    if [[ ! -d /etc/cron.d ]]; then
        mkdir -p /etc/cron.d
    fi

    # Check if cron job already exists
    if [[ ! -f /etc/cron.d/cncnet-monitor ]]; then
        cat > /etc/cron.d/cncnet-monitor << EOF
# CnCNet Server Monitoring
*/5 * * * * root $INSTALL_DIR/health_check.sh || systemctl restart cncnet-server
EOF

        # Set proper permissions for cron file
        chmod 644 /etc/cron.d/cncnet-monitor
    else
        log_info "Cron monitoring job already exists"
    fi

    # Restart cron service if it exists
    if systemctl list-unit-files | grep -q "crond.service"; then
        systemctl restart crond 2>/dev/null || true
    elif systemctl list-unit-files | grep -q "cron.service"; then
        systemctl restart cron 2>/dev/null || true
    fi

    log_success "Monitoring configured"
}

# Configure fail2ban for DDoS protection
configure_fail2ban() {
    log_info "Configuring fail2ban..."

    # Check if fail2ban is installed
    if ! command -v fail2ban-client &> /dev/null; then
        log_warn "fail2ban not installed, skipping configuration"
        return
    fi

    # Check if filter already exists
    if [[ ! -f /etc/fail2ban/filter.d/cncnet.conf ]]; then
        # Create fail2ban filter
        cat > /etc/fail2ban/filter.d/cncnet.conf << 'EOF'
[Definition]
failregex = Rate limit exceeded for IP: <HOST>
            Rate limit exceeded for <HOST>
ignoreregex =
EOF
    else
        log_info "Fail2ban filter already exists"
    fi

    # Check if jail already exists
    if [[ ! -f /etc/fail2ban/jail.d/cncnet.conf ]]; then
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
    else
        log_info "Fail2ban jail already exists"
    fi

    # Restart fail2ban
    systemctl restart fail2ban 2>/dev/null || true
    systemctl enable fail2ban 2>/dev/null || true

    log_success "Fail2ban configured"
}

# Performance tuning specific to ARM64
tune_arm64_performance() {
    log_info "Tuning ARM64 performance..."

    # Set CPU governor to performance if available
    if [[ -d /sys/devices/system/cpu/cpu0/cpufreq ]]; then
        local governor_changed=false
        for cpu in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
            if [[ -w $cpu ]]; then
                current_governor=$(cat $cpu 2>/dev/null || echo "unknown")
                if [[ "$current_governor" != "performance" ]]; then
                    echo performance > $cpu 2>/dev/null && governor_changed=true || true
                fi
            fi
        done

        if [[ "$governor_changed" == "true" ]]; then
            log_info "CPU governor set to performance mode"
        else
            log_info "CPU governor already in performance mode or cannot be changed"
        fi
    fi

    # Disable CPU frequency scaling service if exists
    systemctl stop cpufrequtils 2>/dev/null || true
    systemctl disable cpufrequtils 2>/dev/null || true

    # Set IRQ affinity for network interfaces (if accessible)
    if [[ -r /proc/interrupts ]]; then
        for irq in $(grep -E 'eth|ens|enp' /proc/interrupts | awk '{print $1}' | sed 's/://'); do
            if [[ -w /proc/irq/$irq/smp_affinity ]]; then
                echo ff > /proc/irq/$irq/smp_affinity 2>/dev/null || true
            fi
        done
    fi

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
    # Read maintenance password from config
    local maint_pw=""
    if [[ -f $CONFIG_DIR/server.conf ]]; then
        maint_pw=$(grep MAINT_PASSWORD $CONFIG_DIR/server.conf | cut -d'=' -f2 | cut -d'"' -f2)
    fi

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
    if [[ -n "$maint_pw" ]]; then
        echo "Maintenance Password: $maint_pw" >&2
    fi
    echo >&2
    echo "IMPORTANT: Configure AWS Security Group to allow:" >&2
    echo "  - UDP $V3_PORT (from 0.0.0.0/0)" >&2
    echo "  - TCP/UDP $V2_PORT (from 0.0.0.0/0)" >&2
    echo "  - UDP $P2P_PORT1 (from 0.0.0.0/0)" >&2
    echo "  - UDP $P2P_PORT2 (from 0.0.0.0/0)" >&2
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
    check_existing_installation
    detect_distro

    # System preparation
    update_system
    load_kernel_modules
    install_rust
    configure_system_limits
    configure_firewall

    # User and directories
    create_service_user

    # Build and install
    BUILD_DIR=$(build_project)
    install_application $BUILD_DIR

    # Service configuration
    create_systemd_service
    configure_logging
    configure_monitoring
    configure_fail2ban
    tune_arm64_performance

    # Start services
    start_services

    # Display summary
    display_info

    log_success "Installation completed successfully!"
}

# Run main function
main "$@"