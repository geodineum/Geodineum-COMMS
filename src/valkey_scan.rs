//! ValKey SCAN helper — replaces blocking O(N) `KEYS` calls
//! on hot paths:
//!
//!   src/main.rs:1495                     gNode site-registry sweep
//!   src/consumer/site_discovery.rs:39    discover comms streams
//!   src/consumer/site_discovery.rs:59    gNode site-meta probe
//!   src/settings/store.rs:114            settings key enumeration
//!   src/retry/manager.rs:183             due-retry sweep
//!
//! `KEYS` is blocking O(N) over the entire keyspace and serializes the
//! ValKey main thread — at multi-tenant scale that stalls every other
//! tenant's command for the duration. `SCAN` is cursor-iterated O(1)-
//! per-batch with `COUNT` hint and yields between iterations, so it
//! never starves the main thread.
//!
//! This helper takes a connection + glob pattern and returns the full
//! key list, draining the SCAN cursor to completion. Callers that
//! processed `KEYS` results in a single Vec swap their call to this
//! helper without changing the rest of their flow.

use redis::aio::MultiplexedConnection;

use crate::error::{CommsError, Result};

/// SCAN-iterate every key matching `pattern`. COUNT hint of 500 trades
/// per-iteration cost vs. round-trips; tune with operator data once
/// production keyspace shape is known.
pub async fn scan_keys(conn: &mut MultiplexedConnection, pattern: &str) -> Result<Vec<String>> {
    let mut keys = Vec::new();
    let mut cursor: u64 = 0;
    loop {
        let (new_cursor, batch): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(pattern)
            .arg("COUNT")
            .arg(500)
            .query_async(conn)
            .await
            .map_err(CommsError::ValKey)?;
        keys.extend(batch);
        if new_cursor == 0 {
            break;
        }
        cursor = new_cursor;
    }
    Ok(keys)
}
