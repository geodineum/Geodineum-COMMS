//! Schema registry — publishes COMMS message contracts to ValKey
//!
//! Delegates to the shared geodineum-schema crate for YAML loading and
//! ValKey publication. Contract definitions live in config/schemas/*.yaml.
//!
//! Key pattern: {site_id}:gnode:schema:{component}:{contract_name}
//! Discovery:   {site_id}:gnode:schema:_index → JSON array of all registered schemas

use redis::aio::MultiplexedConnection;
use tracing::{info, warn};

// Re-export types from shared crate for consumers that need them
pub use geodineum_schema::{SchemaField, StreamContract};

/// Publish all COMMS stream contracts to ValKey for discovery.
/// Called on daemon startup (trigger 2 of 3).
pub async fn publish_comms_schemas(
    conn: &mut MultiplexedConnection,
    site_id: &str,
    environment: &str,
) {
    let count = geodineum_schema::publish(
        "config/schemas/",
        conn,
        site_id,
        environment,
    )
    .await;

    if count > 0 {
        info!(count, "COMMS schemas published via geodineum-schema");
    } else {
        warn!("No COMMS schemas published — check config/schemas/ directory");
    }
}
