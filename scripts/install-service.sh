#!/bin/bash
# Install Geodineum-COMMS as a systemd service
#
# SECURITY MODEL:
#   - Runs as geodineum-comms:geodineum-comms (dedicated service user —
#     NOT gnode; a compromised COMMS must not equal a compromised daemon)
#   - Files owned by geodineum-comms with 640 permissions
#   - Members of 'geodineum-comms' group can READ but not WRITE
#   - Password files are 600 (only geodineum-comms user can read)
#
# Prerequisites:
#   - Geodineum-COMMS must be built (cargo build --release)
#   - ACL user must be created (./scripts/setup-acl-user.sh)
#   - 'geodineum-comms' user and group must exist
#
# Usage:
#   sudo ./scripts/install-service.sh [--user <user>] [--environment <env>]

set -euo pipefail

# ============================================
# Configuration
# ============================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMS_DIR="$(dirname "$SCRIPT_DIR")"

# Defaults - run as the dedicated COMMS user (NOT gnode, NOT the operator)
SERVICE_USER="geodineum-comms"
SERVICE_GROUP="geodineum-comms"
ENVIRONMENT="production"
LOG_LEVEL="info"

# Find gNode installation
GNODE_DIR=""
for candidate in "/opt/geodineum/gNode" "/opt/gNode"; do
    if [[ -d "$candidate" ]]; then
        GNODE_DIR="$candidate"
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
        --log-level)
            LOG_LEVEL="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: sudo $0 [OPTIONS]"
            echo ""
            echo "Installs Geodineum-COMMS as a systemd service."
            echo ""
            echo "Options:"
            echo "  --user, -u        User to run service as (default: $SERVICE_USER)"
            echo "  --environment, -e DTAP environment (default: production)"
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
echo "Geodineum-COMMS Service Installation"
echo "=============================================="
echo ""
echo "COMMS Directory: $COMMS_DIR"
echo "GSD Directory: $GNODE_DIR"
echo "Service User: $SERVICE_USER"
echo "Environment: $ENVIRONMENT"
echo ""

# ============================================
# Check Prerequisites
# ============================================

echo "🔍 Checking prerequisites..."

# Check binary exists
BINARY="$COMMS_DIR/target/release/geodineum-comms"
if [[ ! -f "$BINARY" ]]; then
    echo "Error: Binary not found at $BINARY"
    echo "Build first with: cd $COMMS_DIR && cargo build --release"
    exit 1
fi
echo "   ✅ Binary found: $BINARY"

# Check password file exists
PASSWORD_FILE="$COMMS_DIR/.gnode/valkey_comms.password"
if [[ ! -f "$PASSWORD_FILE" ]]; then
    echo "Error: Password file not found at $PASSWORD_FILE"
    echo "Run first: ./scripts/setup-acl-user.sh"
    exit 1
fi
echo "   ✅ Credentials found: $PASSWORD_FILE"

# Check service user exists
if ! id "$SERVICE_USER" &>/dev/null; then
    echo "Error: User $SERVICE_USER does not exist"
    echo "Create it with: sudo useradd --system --gid geodineum-comms --no-create-home -s /usr/sbin/nologin -d /opt/geodineum/Geodineum-COMMS geodineum-comms"
    exit 1
fi
echo "   ✅ User exists: $SERVICE_USER"

# Check service group exists
if ! getent group "$SERVICE_GROUP" &>/dev/null; then
    echo "Error: Group $SERVICE_GROUP does not exist"
    echo "Create it with: sudo groupadd --system geodineum-comms"
    exit 1
fi
echo "   ✅ Group exists: $SERVICE_GROUP"

echo ""

# ============================================
# Create Service File
# ============================================

echo "📝 Creating systemd service..."

SERVICE_FILE="/etc/systemd/system/geodineum-comms.service"

cat > "$SERVICE_FILE" << EOF
[Unit]
Description=Geodineum-COMMS Notification Daemon
Documentation=https://github.com/geodineum/Geodineum-COMMS
After=network.target valkey-gnode.service
Wants=valkey-gnode.service

[Service]
Type=simple
User=$SERVICE_USER
Group=$SERVICE_GROUP
WorkingDirectory=$COMMS_DIR

# Environment
Environment="RUST_LOG=geodineum_comms=$LOG_LEVEL"
Environment="ENVIRONMENT=$ENVIRONMENT"
Environment="LOG_LEVEL=$LOG_LEVEL"

# Load password from file (secure, not in environment)
ExecStart=/bin/bash -c 'exec $BINARY \\
    --redis-auth "\$(cat $PASSWORD_FILE)" \\
    --environment $ENVIRONMENT \\
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
ReadWritePaths=$COMMS_DIR/.gnode $COMMS_DIR/logs

# Resource limits
LimitNOFILE=65536
TasksMax=4096

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=geodineum-comms

[Install]
WantedBy=multi-user.target
EOF

echo "   ✅ Created $SERVICE_FILE"

# ============================================
# Create Log Directory
# ============================================

LOG_DIR="$COMMS_DIR/logs"
mkdir -p "$LOG_DIR"
chown "$SERVICE_USER:$SERVICE_GROUP" "$LOG_DIR"
chmod 750 "$LOG_DIR"  # owner read/write/execute, group read/execute

echo "   ✅ Created log directory: $LOG_DIR"

# ============================================
# Set Permissions ($SERVICE_USER with 640/600)
# ============================================
#
# Security model:
#   - Files owned by the dedicated service user
#   - Password files: 600 (only the service user can read)
#   - Config/code: 640 (owner read/write, group read-only)
#   - Directories: 750 (owner full, group read/execute)
#

echo "🔐 Setting permissions (${SERVICE_USER}:${SERVICE_GROUP}, 640 for files, 750 for dirs)..."

# Password file: 600 (only the service user can read - contains secrets)
chown "$SERVICE_USER:$SERVICE_GROUP" "$PASSWORD_FILE"
chmod 600 "$PASSWORD_FILE"
echo "   ✅ Password file: 600 (${SERVICE_USER} only)"

# .gnode directory: 750 (owner full, group read/execute)
GNODE_DIR="$COMMS_DIR/.gnode"
if [[ -d "$GNODE_DIR" ]]; then
    chown -R "$SERVICE_USER:$SERVICE_GROUP" "$GNODE_DIR"
    chmod 750 "$GNODE_DIR"
    find "$GNODE_DIR" -type f -exec chmod 640 {} \;
    find "$GNODE_DIR" -type d -exec chmod 750 {} \;
    # But password files must be 600
    find "$GNODE_DIR" -name "*.password" -exec chmod 600 {} \;
fi
echo "   ✅ .gnode directory: 750/640 (password files: 600)"

# Config directory: 750/640 (group can read configs)
if [[ -d "$COMMS_DIR/config" ]]; then
    chown -R "$SERVICE_USER:$SERVICE_GROUP" "$COMMS_DIR/config"
    find "$COMMS_DIR/config" -type d -exec chmod 750 {} \;
    find "$COMMS_DIR/config" -type f -exec chmod 640 {} \;
fi
echo "   ✅ Config directory: 750/640"

# Binary: 750 (executable by the service user and group)
chown "$SERVICE_USER:$SERVICE_GROUP" "$BINARY"
chmod 750 "$BINARY"
echo "   ✅ Binary: 750"

# Source/target directories: 750/640 (group can read for debugging)
if [[ -d "$COMMS_DIR/target" ]]; then
    chown -R "$SERVICE_USER:$SERVICE_GROUP" "$COMMS_DIR/target"
    find "$COMMS_DIR/target" -type d -exec chmod 750 {} \;
    find "$COMMS_DIR/target" -type f -exec chmod 640 {} \;
    # Binaries need execute permission
    find "$COMMS_DIR/target" -type f -executable -exec chmod 750 {} \;
fi

echo "   ✅ All permissions set"
echo ""
echo "   📋 Permission summary:"
echo "      - Password files: 600 (${SERVICE_USER} user only)"
echo "      - Config files: 640 (owner read/write, group read)"
echo "      - Directories: 750 (owner full, group read/execute)"
echo "      - Members of '${SERVICE_GROUP}' group can READ but not WRITE"

# ============================================
# Reload and Enable Service
# ============================================

echo "🔄 Configuring systemd..."

systemctl daemon-reload
echo "   ✅ Reloaded systemd"

systemctl enable geodineum-comms
echo "   ✅ Enabled geodineum-comms.service"

echo ""

# ============================================
# Summary
# ============================================

echo "=============================================="
echo "✅ Geodineum-COMMS Service Installed"
echo "=============================================="
echo ""
echo "📋 Service Management:"
echo ""
echo "   Start:   sudo systemctl start geodineum-comms"
echo "   Stop:    sudo systemctl stop geodineum-comms"
echo "   Status:  sudo systemctl status geodineum-comms"
echo "   Logs:    journalctl -u geodineum-comms -f"
echo ""
echo "📋 Start the service now?"
echo ""
read -p "   Start geodineum-comms.service? [y/N] " -n 1 -r
echo ""
if [[ $REPLY =~ ^[Yy]$ ]]; then
    systemctl start geodineum-comms
    sleep 2
    systemctl status geodineum-comms --no-pager
fi
