#!/bin/bash

# CnCNet Server Health Check Script
# Author: Khaled Sameer <khaled.smq@hotmail.com>
# Repository: https://github.com/khaledsmq/cncnet-server
# Description: Comprehensive health check for monitoring and alerting

set -euo pipefail

# Exit codes
readonly EXIT_OK=0
readonly EXIT_WARNING=1
readonly EXIT_CRITICAL=2
readonly EXIT_UNKNOWN=3

# Configuration
readonly SERVICE_NAME="cncnet-server"
readonly SYSTEMD_SERVICE="cncnet-server.service"
readonly LOG_DIR="/var/log/cncnet-server"
readonly CONFIG_FILE="/etc/cncnet-server/server.conf"

# Ports to check
readonly V3_PORT="${V3_PORT:-50001}"
readonly V2_PORT="${V2_PORT:-50000}"
readonly P2P_PORT1="${P2P_PORT1:-8054}"
readonly P2P_PORT2="${P2P_PORT2:-3478}"

# Thresholds
readonly CPU_WARNING=70
readonly CPU_CRITICAL=90
readonly MEM_WARNING=80
readonly MEM_CRITICAL=95
readonly DISK_WARNING=80
readonly DISK_CRITICAL=90

# Colors for output
readonly RED='\033[0;31m'
readonly YELLOW='\033[1;33m'
readonly GREEN='\033[0;32m'
readonly NC='\033[0m' # No Color

# Nagios-compatible output format
output_status() {
    local status=$1
    local message=$2
    local perfdata=${3:-}

    case $status in
        $EXIT_OK)
            echo -e "${GREEN}OK${NC}: $message${perfdata:+ | $perfdata}"
            ;;
        $EXIT_WARNING)
            echo -e "${YELLOW}WARNING${NC}: $message${perfdata:+ | $perfdata}"
            ;;
        $EXIT_CRITICAL)
            echo -e "${RED}CRITICAL${NC}: $message${perfdata:+ | $perfdata}"
            ;;
        *)
            echo -e "UNKNOWN: $message${perfdata:+ | $perfdata}"
            ;;
    esac
}

# Check if service is running
check_service() {
    if systemctl is-active --quiet $SYSTEMD_SERVICE; then
        return 0
    else
        output_status $EXIT_CRITICAL "CnCNet Server service is not running"
        exit $EXIT_CRITICAL
    fi
}

# Check UDP ports
check_udp_ports() {
    local warnings=0
    local errors=0
    local checked_ports=""

    for port in $V3_PORT $V2_PORT $P2P_PORT1 $P2P_PORT2; do
        if ss -uln | grep -q ":$port "; then
            checked_ports="$checked_ports UDP:$port=OK"
        else
            errors=$((errors + 1))
            checked_ports="$checked_ports UDP:$port=FAIL"
        fi
    done

    if [ $errors -gt 0 ]; then
        return 1
    fi
    return 0
}

# Check TCP ports
check_tcp_ports() {
    if ss -tln | grep -q ":$V2_PORT "; then
        return 0
    else
        return 1
    fi
}

# Check CPU usage
check_cpu() {
    local cpu_usage=$(top -bn1 | grep "Cpu(s)" | awk '{print $2}' | cut -d'%' -f1 | cut -d'.' -f1)

    if [ -z "$cpu_usage" ]; then
        # Alternative method using ps
        cpu_usage=$(ps aux | awk -v pid=$(pgrep -f $SERVICE_NAME) '$2 == pid {print $3}' | cut -d'.' -f1)
    fi

    echo "$cpu_usage"
}

# Check memory usage
check_memory() {
    local mem_total=$(free -m | awk 'NR==2{print $2}')
    local mem_used=$(free -m | awk 'NR==2{print $3}')
    local mem_percent=$((mem_used * 100 / mem_total))

    echo "$mem_percent"
}

# Check disk usage
check_disk() {
    local disk_usage=$(df -h / | awk 'NR==2{print $5}' | sed 's/%//')
    echo "$disk_usage"
}

# Check active connections (estimate from log)
check_connections() {
    if [ -f "$LOG_DIR/server.log" ]; then
        # Count unique IPs in last 1000 lines
        local connections=$(tail -1000 "$LOG_DIR/server.log" 2>/dev/null | \
            grep -E "connected from|Client.*connected" | \
            awk '{print $NF}' | sort -u | wc -l)
        echo "$connections"
    else
        echo "0"
    fi
}

# Check for recent errors
check_errors() {
    if [ -f "$LOG_DIR/error.log" ]; then
        local recent_errors=$(find "$LOG_DIR/error.log" -mmin -5 -exec grep -c ERROR {} \; 2>/dev/null || echo 0)
        echo "$recent_errors"
    else
        echo "0"
    fi
}

# Check process info
check_process() {
    local pid=$(pgrep -f $SERVICE_NAME)
    if [ -n "$pid" ]; then
        local proc_info=$(ps -p $pid -o pid,vsz,rss,etime,cmd --no-headers)
        local uptime=$(echo "$proc_info" | awk '{print $4}')
        local mem_kb=$(echo "$proc_info" | awk '{print $3}')
        local mem_mb=$((mem_kb / 1024))

        echo "PID=$pid UPTIME=$uptime MEM=${mem_mb}MB"
    else
        echo "Process not found"
    fi
}

# Main health check logic
main() {
    local overall_status=$EXIT_OK
    local status_messages=""
    local perf_data=""

    # Check if service is running
    check_service

    # Check ports
    if ! check_udp_ports; then
        overall_status=$EXIT_CRITICAL
        status_messages="$status_messages UDP ports not listening."
    fi

    if ! check_tcp_ports; then
        if [ $overall_status -lt $EXIT_WARNING ]; then
            overall_status=$EXIT_WARNING
        fi
        status_messages="$status_messages TCP port $V2_PORT not listening."
    fi

    # Check system resources
    local cpu_usage=$(check_cpu)
    local mem_usage=$(check_memory)
    local disk_usage=$(check_disk)

    # CPU check
    if [ "$cpu_usage" -ge "$CPU_CRITICAL" ]; then
        overall_status=$EXIT_CRITICAL
        status_messages="$status_messages CPU usage critical: ${cpu_usage}%."
    elif [ "$cpu_usage" -ge "$CPU_WARNING" ]; then
        if [ $overall_status -lt $EXIT_WARNING ]; then
            overall_status=$EXIT_WARNING
        fi
        status_messages="$status_messages CPU usage high: ${cpu_usage}%."
    fi

    # Memory check
    if [ "$mem_usage" -ge "$MEM_CRITICAL" ]; then
        overall_status=$EXIT_CRITICAL
        status_messages="$status_messages Memory usage critical: ${mem_usage}%."
    elif [ "$mem_usage" -ge "$MEM_WARNING" ]; then
        if [ $overall_status -lt $EXIT_WARNING ]; then
            overall_status=$EXIT_WARNING
        fi
        status_messages="$status_messages Memory usage high: ${mem_usage}%."
    fi

    # Disk check
    if [ "$disk_usage" -ge "$DISK_CRITICAL" ]; then
        overall_status=$EXIT_CRITICAL
        status_messages="$status_messages Disk usage critical: ${disk_usage}%."
    elif [ "$disk_usage" -ge "$DISK_WARNING" ]; then
        if [ $overall_status -lt $EXIT_WARNING ]; then
            overall_status=$EXIT_WARNING
        fi
        status_messages="$status_messages Disk usage high: ${disk_usage}%."
    fi

    # Check for errors
    local error_count=$(check_errors)
    if [ "$error_count" -gt 10 ]; then
        if [ $overall_status -lt $EXIT_WARNING ]; then
            overall_status=$EXIT_WARNING
        fi
        status_messages="$status_messages Recent errors detected: $error_count."
    fi

    # Get additional metrics
    local connections=$(check_connections)
    local proc_info=$(check_process)

    # Build performance data
    perf_data="cpu=${cpu_usage}%;${CPU_WARNING};${CPU_CRITICAL};0;100"
    perf_data="$perf_data memory=${mem_usage}%;${MEM_WARNING};${MEM_CRITICAL};0;100"
    perf_data="$perf_data disk=${disk_usage}%;${DISK_WARNING};${DISK_CRITICAL};0;100"
    perf_data="$perf_data connections=$connections errors=$error_count"

    # Output final status
    if [ $overall_status -eq $EXIT_OK ]; then
        output_status $EXIT_OK "CnCNet Server is healthy. $proc_info" "$perf_data"
    else
        output_status $overall_status "${status_messages} $proc_info" "$perf_data"
    fi

    exit $overall_status
}

# Parse command line arguments
while getopts "hvV:W:P:" opt; do
    case $opt in
        h)
            echo "Usage: $0 [-h] [-v] [-V port] [-W port] [-P port]"
            echo "  -h        Show this help"
            echo "  -v        Verbose output"
            echo "  -V port   V3 port (default: 50001)"
            echo "  -W port   V2 port (default: 50000)"
            echo "  -P port   P2P port (default: 8054)"
            exit 0
            ;;
        v)
            set -x
            ;;
        V)
            V3_PORT=$OPTARG
            ;;
        W)
            V2_PORT=$OPTARG
            ;;
        P)
            P2P_PORT1=$OPTARG
            ;;
        \?)
            echo "Invalid option: -$OPTARG" >&2
            exit 1
            ;;
    esac
done

# Run main health check
main