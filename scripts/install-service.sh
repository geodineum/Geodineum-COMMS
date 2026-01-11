#!/bin/bash
# Install GSD-COMMS as a systemd service
#
# Prerequisites:
#   - GSD-COMMS must be built (cargo build --release)
#   - ACL user must be created (./scripts/setup-acl-user.sh)
#
# Usage:
#   sudo ./scripts/install-service.sh [--user <user>] [--environment <env>]

set -euo pipefail

# ============================================
# Configuration
# ============================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMS_DIR="$(dirname "$SCRIPT_DIR")"

# Defaults
SERVICE_USER="${USER:-august}"
ENVIRONMENT="production"
API_PORT="8080"
API_BIND="127.0.0.1"
LOG_LEVEL="info"

# Find GSD installation
GSD_DIR=""
for candidate in "/opt/geodineum/GSD" "/opt/GSD"; do
    if [[ -d "$candidate" ]]; then
        GSD_DIR="$candidate"
        break
    fi
done

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --user|-u)
            SERVICE_USER="$2"
            shift 2
            ;;
        --environment|-e)
            ENVIRONMENT="$2"
            shift 2
            ;;
        --api-port)
            API_PORT="$2"
            shift 2
            ;;
        --api-bind)
            API_BIND="$2"
            shift 2
            ;;
        --log-level)
            LOG_LEVEL="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: sudo $0 [OPTIONS]"
            echo ""
            echo "Installs GSD-COMMS as a systemd service."
            echo ""
            echo "Options:"
            echo "  --user, -u        User to run service as (default: $SERVICE_USER)"
            echo "  --environment, -e DTAP environment (default: production)"
            echo "  --api-port        Dashboard API port (default: 8080)"
            echo "  --api-bind        Dashboard bind address (default: 127.0.0.1)"
            echo "  --log-level       Log level (default: info)"
            exit 0
            ;;
        *)
            echo "Error: Unknown argument: $1"
            exit 1
            ;;
    esac
done

# Check root
if [[ $EUID -ne 0 ]]; then
    echo "Error: This script must be run as root (use sudo)"
    exit 1
fi

echo "=============================================="
echo "GSD-COMMS Service Installation"
echo "=============================================="
echo ""
echo "COMMS Directory: $COMMS_DIR"
echo "GSD Directory: $GSD_DIR"
echo "Service User: $SERVICE_USER"
echo "Environment: $ENVIRONMENT"
echo "API: $API_BIND:$API_PORT"
echo ""

# ============================================
# Check Prerequisites
# ============================================

echo "🔍 Checking prerequisites..."

# Check binary exists
BINARY="$COMMS_DIR/target/release/gsd-comms"
if [[ ! -f "$BINARY" ]]; then
    echo "Error: Binary not found at $BINARY"
    echo "Build first with: cd $COMMS_DIR && cargo build --release"
    exit 1
fi
echo "   ✅ Binary found: $BINARY"

# Check password file exists
PASSWORD_FILE="$COMMS_DIR/.gsd/valkey_comms.password"
if [[ ! -f "$PASSWORD_FILE" ]]; then
    echo "Error: Password file not found at $PASSWORD_FILE"
    echo "Run first: ./scripts/setup-acl-user.sh"
    exit 1
fi
echo "   ✅ Credentials found: $PASSWORD_FILE"

# Check user exists
if ! id "$SERVICE_USER" &>/dev/null; then
    echo "Error: User $SERVICE_USER does not exist"
    exit 1
fi
echo "   ✅ User exists: $SERVICE_USER"

echo ""

# ============================================
# Create Service File
# ============================================

echo "📝 Creating systemd service..."

SERVICE_FILE="/etc/systemd/system/gsd-comms.service"

cat > "$SERVICE_FILE" << EOF
[Unit]
Description=GSD-COMMS Notification Daemon
Documentation=https://github.com/nierto/GSD-COMMS
After=network.target valkey-gsd.service
Wants=valkey-gsd.service

[Service]
Type=simple
User=$SERVICE_USER
Group=$SERVICE_USER
WorkingDirectory=$COMMS_DIR

# Environment
Environment="RUST_LOG=gsd_comms=$LOG_LEVEL"
Environment="ENVIRONMENT=$ENVIRONMENT"
Environment="API_PORT=$API_PORT"
Environment="API_BIND=$API_BIND"
Environment="LOG_LEVEL=$LOG_LEVEL"

# Load password from file (secure, not in environment)
ExecStart=/bin/bash -c 'exec $BINARY \\
    --redis-auth "\$(cat $PASSWORD_FILE)" \\
    --environment $ENVIRONMENT \\
    --api-port $API_PORT \\
    --api-bind $API_BIND \\
    --log-level $LOG_LEVEL \\
    start'

# Restart policy
Restart=on-failure
RestartSec=5
StartLimitBurst=5
StartLimitIntervalSec=60

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=true
ReadWritePaths=$COMMS_DIR/.gsd $COMMS_DIR/logs

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=gsd-comms

[Install]
WantedBy=multi-user.target
EOF

echo "   ✅ Created $SERVICE_FILE"

# ============================================
# Create Log Directory
# ============================================

LOG_DIR="$COMMS_DIR/logs"
mkdir -p "$LOG_DIR"
chown "$SERVICE_USER:$SERVICE_USER" "$LOG_DIR"
chmod 750 "$LOG_DIR"

echo "   ✅ Created log directory: $LOG_DIR"

# ============================================
# Set Permissions
# ============================================

echo "🔐 Setting permissions..."

# Ensure service user can read password file
chown "$SERVICE_USER:$SERVICE_USER" "$PASSWORD_FILE"
chmod 600 "$PASSWORD_FILE"

# Ensure service user can read config
chown -R "$SERVICE_USER:$SERVICE_USER" "$COMMS_DIR/config" 2>/dev/null || true

echo "   ✅ Permissions set"

# ============================================
# Reload and Enable Service
# ============================================

echo "🔄 Configuring systemd..."

systemctl daemon-reload
echo "   ✅ Reloaded systemd"

systemctl enable gsd-comms
echo "   ✅ Enabled gsd-comms.service"

echo ""

# ============================================
# Summary
# ============================================

echo "=============================================="
echo "✅ GSD-COMMS Service Installed"
echo "=============================================="
echo ""
echo "📋 Service Management:"
echo ""
echo "   Start:   sudo systemctl start gsd-comms"
echo "   Stop:    sudo systemctl stop gsd-comms"
echo "   Status:  sudo systemctl status gsd-comms"
echo "   Logs:    journalctl -u gsd-comms -f"
echo ""
echo "🌐 Dashboard:"
echo ""
echo "   URL: http://$API_BIND:$API_PORT/dashboard"
echo ""
echo "   If binding to localhost, use SSH tunnel or nginx proxy for remote access:"
echo "   ssh -L $API_PORT:localhost:$API_PORT user@server"
echo ""
echo "📋 Start the service now?"
echo ""
read -p "   Start gsd-comms.service? [y/N] " -n 1 -r
echo ""
if [[ $REPLY =~ ^[Yy]$ ]]; then
    systemctl start gsd-comms
    sleep 2
    systemctl status gsd-comms --no-pager
fi
