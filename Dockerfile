# Docker Compose configuration for CnCNet Server
# Author: Khaled Sameer <khaled.smq@hotmail.com>
# Repository: https://github.com/khaledsmq/cncnet-server

version: '3.8'

services:
  cncnet-server:
    build:
      context: .
      dockerfile: Dockerfile.arm64
      args:
        RUST_VERSION: stable
    image: cncnet-server:latest
    container_name: cncnet-server
    restart: always
    network_mode: host  # Required for UDP performance

    # Resource limits
    deploy:
      resources:
        limits:
          cpus: '4'
          memory: 2G
        reservations:
          cpus: '2'
          memory: 512M

    # Security settings
    security_opt:
      - no-new-privileges:true
    read_only: true

    # Volumes
    volumes:
      - ./logs:/var/log/cncnet-server:rw
      - ./config:/etc/cncnet-server:ro

    # Environment variables
    environment:
      - RUST_LOG=${RUST_LOG:-info}
      - RUST_BACKTRACE=1
      - SERVER_NAME=${SERVER_NAME:-CnCNet Production Server}
      - V3_PORT=${V3_PORT:-50001}
      - V2_PORT=${V2_PORT:-50000}
      - MAX_CLIENTS=${MAX_CLIENTS:-1000}
      - IP_LIMIT_V3=${IP_LIMIT_V3:-8}
      - IP_LIMIT_V2=${IP_LIMIT_V2:-4}
      - MASTER_URL=${MASTER_URL:-http://cncnet.org/master-announce}
      - NO_MASTER=${NO_MASTER:-false}

    # Health check
    healthcheck:
      test: ["CMD", "/app/health_check.sh"]
      interval: 30s
      timeout: 10s
      retries: 3
      start_period: 10s

    # Logging
    logging:
      driver: "json-file"
      options:
        max-size: "100m"
        max-file: "10"
        compress: "true"

    # Command with environment variable substitution
    command: >
      /app/cncnet-server
      --port ${V3_PORT}
      --portv2 ${V2_PORT}
      --name "${SERVER_NAME}"
      --maxclients ${MAX_CLIENTS}
      --iplimit ${IP_LIMIT_V3}
      --iplimitv2 ${IP_LIMIT_V2}
      --master "${MASTER_URL}"
      --maintpw "${MAINT_PASSWORD}"
      $$([ "$${NO_MASTER}" = "true" ] && echo "--nomaster")

  # Optional: Prometheus node exporter for monitoring
  node-exporter:
    image: prom/node-exporter:latest
    container_name: cncnet-node-exporter
    restart: always
    network_mode: host
    pid: host
    volumes:
      - /proc:/host/proc:ro
      - /sys:/host/sys:ro
      - /:/rootfs:ro
    command:
      - '--path.procfs=/host/proc'
      - '--path.sysfs=/host/sys'
      - '--path.rootfs=/rootfs'
      - '--collector.filesystem.mount-points-exclude=^/(sys|proc|dev|host|etc)($$|/)'
      - '--collector.netclass.ignored-devices=^(veth.*|br.*|docker.*|virbr.*)$$'

  # Optional: Grafana for visualization
  grafana:
    image: grafana/grafana:latest
    container_name: cncnet-grafana
    restart: always
    ports:
      - "3000:3000"
    volumes:
      - grafana-storage:/var/lib/grafana
      - ./grafana/provisioning:/etc/grafana/provisioning:ro
    environment:
      - GF_SECURITY_ADMIN_PASSWORD=${GRAFANA_PASSWORD:-admin}
      - GF_INSTALL_PLUGINS=grafana-clock-panel,grafana-simple-json-datasource

volumes:
  grafana-storage: