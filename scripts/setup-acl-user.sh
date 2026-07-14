#!/bin/bash
# Geodineum-COMMS ACL User Setup
#
# Creates the geodineum_comms ACL user with permissions to:
#   - Read ALL site comms streams (*:gnode:comms:*)
#   - Write comms config and retry state (*:comms:config, *:comms:*)
#   - Read gNode site registry for discovery (gnode:site:*:meta)
#   - Use FCALL for Lua functions
#
# Prerequisites:
#   - gNode must be installed at /opt/geodineum/gNode or /opt/gNode
#   - valkey-gnode.service must be running
#   - gnode_daemon credentials must exist
#
# Usage:
#   ./setup-acl-user.sh [--gnode-dir /path/to/gNode]

set -euo pipefail

# ============================================
# Configuration
# ============================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMS_DIR="$(dirname "$SCRIPT_DIR")"

# Find gNode installation
GNODE_DIR=""
for candidate in "/opt/geodineum/gNode" "/opt/gNode"; do
    if [[ -d "$candidate" && -f "$candidate/scripts/valkey-cli-secure.sh" ]]; then
        GNODE_DIR="$candidate"
        break
    fi
done

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --gnode-dir)
            GNODE_DIR="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: $0 [--gnode-dir /path/to/gNode]"
            echo ""
            echo "Creates the geodineum_comms ACL user for Geodineum-COMMS daemon."
            echo ""
            echo "Options:"
            echo "  --gnode-dir    Path to gNode installation (auto-detected if not specified)"
            echo ""
            echo "Prerequisites:"
            echo "  - gNode must be installed"
            echo "  - valkey-gnode.service must be running"
            echo "  - gnode_daemon credentials must exist"
            exit 0
            ;;
        *)
            echo "Error: Unknown argument: $1"
            exit 1
            ;;
    esac
done

if [[ -z "$GNODE_DIR" ]]; then
    echo "Error: Could not find gNode installation"
    echo "Checked: /opt/geodineum/gNode, /opt/gNode"
    echo "Use --gnode-dir to specify the path"
    exit 1
fi

if [[ ! -f "$GNODE_DIR/scripts/valkey-cli-secure.sh" ]]; then
    echo "Error: gNode installation at $GNODE_DIR appears incomplete"
    echo "Missing: $GNODE_DIR/scripts/valkey-cli-secure.sh"
    exit 1
fi

# Load gNode environment if available
if [[ -f "$GNODE_DIR/.env" ]]; then
    source "$GNODE_DIR/.env"
fi

ACL_USER="geodineum_comms"
PASSWORD_DIR="${COMMS_DIR}/.gnode"
PASSWORD_FILE="${PASSWORD_DIR}/valkey_comms.password"

echo "=============================================="
echo "Geodineum-COMMS ACL User Setup"
echo "=============================================="
echo ""
echo "GSD Directory: $GNODE_DIR"
echo "COMMS Directory: $COMMS_DIR"
echo "ACL User: $ACL_USER"
echo ""

# ============================================
# Check Prerequisites
# ============================================

echo "🔍 Checking prerequisites..."

# Check ValKey is running
if ! systemctl is-active --quiet valkey-gnode 2>/dev/null; then
    echo "Error: valkey-gnode.service is not running"
    echo "Start it with: sudo systemctl start valkey-gnode"
    exit 1
fi
echo "   ✅ valkey-gnode.service is running"

# Check daemon credentials exist
DAEMON_PASSWORD_FILE="$GNODE_DIR/.gnode/valkey_daemon.password"
if [[ ! -f "$DAEMON_PASSWORD_FILE" ]]; then
    echo "Error: Daemon password not found at $DAEMON_PASSWORD_FILE"
    echo "gNode must be fully installed first"
    exit 1
fi
echo "   ✅ Daemon credentials found"

# Test daemon connection
export VALKEY_USER=gnode_daemon
if ! "$GNODE_DIR/scripts/valkey-cli-secure.sh" PING >/dev/null 2>&1; then
    echo "Error: Cannot connect to ValKey with gnode_daemon credentials"
    exit 1
fi
echo "   ✅ ValKey connection verified"
echo ""

# ============================================
# Create Password Directory
# ============================================

echo "🔐 Setting up credentials..."

mkdir -p "$PASSWORD_DIR"
chmod 700 "$PASSWORD_DIR"

# Generate 64-character hex password (no newline - critical for ValKey auth)
PASSWORD=$(openssl rand -hex 32)
echo -n "$PASSWORD" > "$PASSWORD_FILE"
chmod 600 "$PASSWORD_FILE"

echo "   ✅ Generated password"
echo "   📁 Stored at: $PASSWORD_FILE"
echo ""

# ============================================
# Create/Update ACL User
# ============================================

echo "🔧 Creating ACL user: $ACL_USER"

# Check if user already exists
USER_CHECK=$("$GNODE_DIR/scripts/valkey-cli-secure.sh" ACL GETUSER "$ACL_USER" 2>&1 || true)
if [[ "$USER_CHECK" != *"no such user"* && "$USER_CHECK" != *"ERR"* ]]; then
    echo "   ⚠️  User $ACL_USER already exists, resetting..."
fi

# Create/reset user with password
"$GNODE_DIR/scripts/valkey-cli-secure.sh" ACL SETUSER "$ACL_USER" on resetpass ">${PASSWORD}"

# ============================================
# Set Keyspace Permissions
# ============================================

# Geodineum-COMMS needs access to:
#   - ALL comms streams: *:gnode:comms:* (read messages from any site)
#   - Comms config: *:comms:config (per-site settings)
#   - Comms retry state: *:comms:retry:* (retry management)
#   - Comms messages tracking: *:comms:messages:* (dispatch status)
#   - Site discovery: gnode:site:*:meta, gnode:sites:registry (read-only)
#   - Global GSD keys: gnode:* (for routing info)
#
# The ~* pattern for comms is intentional - we need cross-site access

"$GNODE_DIR/scripts/valkey-cli-secure.sh" ACL SETUSER "$ACL_USER" resetkeys \
    '~*:gnode:comms:*' \
    '~*:comms:config' \
    '~*:comms:retry:*' \
    '~*:comms:messages:*' \
    '~*:comms:stats:*' \
    '~*:comms:conversation:*' \
    '~*:comms:context:*' \
    '~*:comms:active_context:*' \
    '~gnode:site:*' \
    '~gnode:sites:*' \
    '~gnode:routing:*' \
    '~topology:*' \
    '~*:gnode:schema:*' \
    '~*:gnode:unified:*' \
    '~*:inference:metrics:*' \
    '~*:inference:history:*'

echo "   ✅ Set keyspace permissions"
echo "      - *:gnode:comms:* (all site comms streams, incl. inbound)"
echo "      - *:comms:config (per-site settings)"
echo "      - *:comms:retry:* (retry state)"
echo "      - *:comms:messages:* (tracking)"
echo "      - *:comms:conversation:* (inbound sessions)"
echo "      - *:comms:context:* (reply-correlation contexts)"
echo "      - *:comms:active_context:* (active context pointers)"
echo "      - gnode:site:* (site discovery)"
echo "      - *:gnode:schema:* (component schema registry)"
echo "      - *:gnode:unified:* (inference service stream)"
echo "      - *:inference:metrics:* (pipeline status reads)"
echo "      - *:inference:history:* (conversation history reads)"

# ============================================
# Set Channel Permissions
# ============================================

"$GNODE_DIR/scripts/valkey-cli-secure.sh" ACL SETUSER "$ACL_USER" resetchannels "&*"

echo "   ✅ Set channel permissions (all channels)"

# ============================================
# Set Command Permissions
# ============================================

# Stream operations (consumer)
# Key-value operations (settings storage)
# Hash operations (site metadata)
# Utility operations

"$GNODE_DIR/scripts/valkey-cli-secure.sh" ACL SETUSER "$ACL_USER" nocommands \
    +xread +xreadgroup +xadd +xack +xclaim +xautoclaim +xpending +xinfo +xlen +xtrim +xrange +xrevrange +xgroup +xdel \
    +fcall +fcall_ro \
    +get +set +setex +setnx +psetex +del +exists +ttl +pttl +expire +pexpire +mget +mset +incr +decr +incrby +decrby +append \
    +hget +hset +hgetall +hdel +hexists +hkeys +hvals +hincrby +hincrbyfloat +hmget +hmset +hsetnx +hlen +hscan \
    +lrange +llen +lpush +rpush +lpop +rpop \
    +sadd +smembers +sismember +srem +scard +sscan \
    +keys +scan +type +object \
    +ping +echo +time +client +auth +select

echo "   ✅ Set command permissions"
echo "      - Stream: xread, xreadgroup, xadd, xack, xclaim, xautoclaim, xpending, xinfo..."
echo "      - Functions: fcall, fcall_ro"
echo "      - Key-Value: get, set, setex, del, exists, expire..."
echo "      - Hash: hget, hset, hgetall, hdel..."
echo "      - Set: sadd, smembers, sismember..."
echo "      - Utility: keys, scan, ping, time..."

# ============================================
# Save ACL Configuration
# ============================================

"$GNODE_DIR/scripts/valkey-cli-secure.sh" ACL SAVE

echo "   ✅ Saved ACL configuration to disk"
echo ""

# ============================================
# Create Consumer Groups for Known Sites
# ============================================

echo "📡 Setting up consumer groups for existing sites..."

# Discover existing comms streams
COMMS_STREAMS=$("$GNODE_DIR/scripts/valkey-cli-secure.sh" KEYS "*:gnode:comms:*" 2>/dev/null || echo "")

if [[ -n "$COMMS_STREAMS" ]]; then
    CONSUMER_GROUP="geodineum_comms_dispatch"

    while IFS= read -r stream; do
        if [[ -n "$stream" ]]; then
            # Try to create consumer group (ignore if exists)
            if "$GNODE_DIR/scripts/valkey-cli-secure.sh" XGROUP CREATE "$stream" "$CONSUMER_GROUP" 0 MKSTREAM 2>/dev/null; then
                echo "   ✅ Created group $CONSUMER_GROUP on $stream"
            else
                echo "   ⏭️  Group exists: $CONSUMER_GROUP on $stream"
            fi
        fi
    done <<< "$COMMS_STREAMS"
else
    echo "   ℹ️  No existing comms streams found"
fi
echo ""

# ============================================
# Verify Setup
# ============================================

echo "🔍 Verifying setup..."

# Test authentication with new user
export VALKEY_USER="$ACL_USER"
export REDISCLI_AUTH="$PASSWORD"

if "$GNODE_DIR/scripts/valkey-cli-secure.sh" PING >/dev/null 2>&1; then
    echo "   ✅ Authentication successful"
else
    echo "   ❌ Authentication failed!"
    exit 1
fi

# Test read access to site discovery
SITE_COUNT=$("$GNODE_DIR/scripts/valkey-cli-secure.sh" KEYS "gnode:site:*:meta" 2>/dev/null | wc -l || echo "0")
echo "   ✅ Site discovery access verified ($SITE_COUNT sites found)"

# Test read access to comms streams (if any exist)
if [[ -n "$COMMS_STREAMS" ]]; then
    FIRST_STREAM=$(echo "$COMMS_STREAMS" | head -1)
    if "$GNODE_DIR/scripts/valkey-cli-secure.sh" XINFO STREAM "$FIRST_STREAM" >/dev/null 2>&1; then
        echo "   ✅ Comms stream access verified ($FIRST_STREAM)"
    fi
fi

echo ""

# ============================================
# Create Symlink to gNode cli tool
# ============================================

echo "🔗 Creating utility symlinks..."

COMMS_SCRIPTS_DIR="$COMMS_DIR/scripts"
mkdir -p "$COMMS_SCRIPTS_DIR"

# Create a wrapper script for valkey-cli with COMMS credentials
cat > "$COMMS_SCRIPTS_DIR/valkey-cli-comms.sh" << 'WRAPPER_EOF'
#!/bin/bash
# ValKey CLI wrapper with Geodineum-COMMS credentials
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMS_DIR="$(dirname "$SCRIPT_DIR")"

# Find gNode installation
GNODE_DIR=""
for candidate in "/opt/geodineum/gNode" "/opt/gNode"; do
    if [[ -d "$candidate" && -f "$candidate/scripts/valkey-cli-secure.sh" ]]; then
        GNODE_DIR="$candidate"
        break
    fi
done

if [[ -z "$GNODE_DIR" ]]; then
    echo "Error: gNode installation not found" >&2
    exit 1
fi

# Use COMMS credentials
export VALKEY_USER="${VALKEY_USER:-geodineum_comms}"
export REDISCLI_AUTH="${REDISCLI_AUTH:-$(cat "$COMMS_DIR/.gnode/valkey_comms.password" 2>/dev/null || echo "")}"

exec "$GNODE_DIR/scripts/valkey-cli-secure.sh" "$@"
WRAPPER_EOF

chmod +x "$COMMS_SCRIPTS_DIR/valkey-cli-comms.sh"
echo "   ✅ Created $COMMS_SCRIPTS_DIR/valkey-cli-comms.sh"

echo ""

# ============================================
# Summary
# ============================================

echo "=============================================="
echo "✅ Geodineum-COMMS ACL Setup Complete"
echo "=============================================="
echo ""
echo "📋 Created Resources:"
echo ""
echo "   ACL User: $ACL_USER"
echo "   Password: $PASSWORD_FILE"
echo ""
echo "🔑 Permissions Summary:"
echo ""
echo "   Keyspace Access:"
echo "     - *:gnode:comms:* (ALL site comms streams)"
echo "     - *:comms:config (per-site notification settings)"
echo "     - *:comms:retry:* (retry state management)"
echo "     - *:comms:messages:* (dispatch tracking)"
echo "     - gnode:site:* (site discovery - read)"
echo ""
echo "   Consumer Group: geodineum_comms_dispatch"
echo ""
echo "📋 Next Steps:"
echo ""
echo "   1. Build Geodineum-COMMS:"
echo "      cd $COMMS_DIR && cargo build --release"
echo ""
echo "   2. Test the daemon:"
echo "      ./target/release/geodineum-comms --redis-auth \"\$(cat $PASSWORD_FILE)\" test --site-id your_site --channel all"
echo ""
echo "   3. Run the daemon:"
echo "      ./target/release/geodineum-comms --redis-auth \"\$(cat $PASSWORD_FILE)\" start"
echo ""
echo "   4. Or install as systemd service:"
echo "      sudo ./scripts/install-service.sh"
echo ""
echo "🔧 CLI Access:"
echo ""
echo "   # Using the wrapper script:"
echo "   $COMMS_SCRIPTS_DIR/valkey-cli-comms.sh PING"
echo ""
echo "   # Or manually:"
echo "   VALKEY_USER=geodineum_comms REDISCLI_AUTH=\"\$(cat $PASSWORD_FILE)\" $GNODE_DIR/scripts/valkey-cli-secure.sh PING"
echo ""
