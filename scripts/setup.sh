#!/bin/bash
# GSD-COMMS Full Setup
#
# One-command setup for GSD-COMMS:
#   1. Creates ACL user
#   2. Builds the daemon
#   3. Optionally installs systemd service
#
# Prerequisites:
#   - GSD must be installed
#   - Rust/Cargo must be installed
#   - valkey-gsd.service must be running
#
# Usage:
#   ./scripts/setup.sh [--install-service]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMS_DIR="$(dirname "$SCRIPT_DIR")"

INSTALL_SERVICE=false

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --install-service|-s)
            INSTALL_SERVICE=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Complete GSD-COMMS setup."
            echo ""
            echo "Options:"
            echo "  --install-service, -s   Also install systemd service (requires sudo)"
            exit 0
            ;;
        *)
            echo "Error: Unknown argument: $1"
            exit 1
            ;;
    esac
done

echo "=============================================="
echo "GSD-COMMS Setup"
echo "=============================================="
echo ""

# ============================================
# Step 1: ACL User
# ============================================

echo "📋 Step 1/3: Creating ACL user..."
echo ""

"$SCRIPT_DIR/setup-acl-user.sh"

echo ""

# ============================================
# Step 2: Build
# ============================================

echo "📋 Step 2/3: Building daemon..."
echo ""

cd "$COMMS_DIR"

if ! command -v cargo &> /dev/null; then
    echo "Error: Cargo not found. Install Rust first:"
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

echo "   Running: cargo build --release"
echo "   This may take a few minutes on first build..."
echo ""

cargo build --release

echo ""
echo "   ✅ Build complete: $COMMS_DIR/target/release/gsd-comms"
echo ""

# ============================================
# Step 3: Service (optional)
# ============================================

if [[ "$INSTALL_SERVICE" == "true" ]]; then
    echo "📋 Step 3/3: Installing systemd service..."
    echo ""

    if [[ $EUID -ne 0 ]]; then
        echo "   Installing service requires sudo..."
        sudo "$SCRIPT_DIR/install-service.sh"
    else
        "$SCRIPT_DIR/install-service.sh"
    fi
else
    echo "📋 Step 3/3: Service installation skipped"
    echo ""
    echo "   To install as a service later, run:"
    echo "   sudo $SCRIPT_DIR/install-service.sh"
fi

echo ""

# ============================================
# Summary
# ============================================

echo "=============================================="
echo "✅ GSD-COMMS Setup Complete"
echo "=============================================="
echo ""
echo "📋 Quick Start:"
echo ""
echo "   # Test channels:"
echo "   ./target/release/gsd-comms --redis-auth \"\$(cat .gsd/valkey_comms.password)\" test --site-id staging_nierto_com --channel all"
echo ""
echo "   # Run daemon (foreground):"
echo "   ./target/release/gsd-comms --redis-auth \"\$(cat .gsd/valkey_comms.password)\" start"
echo ""

if [[ "$INSTALL_SERVICE" == "true" ]]; then
    echo "   # Or use systemd:"
    echo "   sudo systemctl start gsd-comms"
    echo "   journalctl -u gsd-comms -f"
else
    echo "   # Or install as service:"
    echo "   sudo ./scripts/install-service.sh"
fi

echo ""
echo "🌐 Dashboard: http://localhost:8080/dashboard"
echo ""
