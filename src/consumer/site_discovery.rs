//! Dynamic site discovery from gNode

use redis::aio::MultiplexedConnection;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::error::Result;

/// Discovers registered sites from gNode ValKey
pub struct SiteDiscovery {
    conn: MultiplexedConnection,
    known_sites: Arc<RwLock<HashSet<String>>>,
    environment: String,
}

impl SiteDiscovery {
    pub fn new(conn: MultiplexedConnection, environment: String) -> Self {
        Self {
            conn,
            known_sites: Arc::new(RwLock::new(HashSet::new())),
            environment,
        }
    }

    /// Discover all sites with comms streams
    pub async fn discover_sites(&mut self) -> Result<Vec<String>> {
        // Pattern to find comms streams. Keys use brace-literal hash-tag
        // form {site_id}:gnode:comms:{env} so ValKey Cluster routes every
        // per-site key (stream + retry + conversation + settings) to the
        // same hash-slot. KEYS glob `{*}:...` matches the brace-literal
        // form only — legacy unbraced keys are intentionally ignored per
        // pre-release clean-break policy (dev-box upgrades documented in
        // §4 of REMEDIATION_PLAN_DEEP.md).
        let pattern = format!("{{*}}:gnode:comms:{}", self.environment);

        // an earlier hardening pass: SCAN cursor instead of blocking KEYS.
        let keys = crate::valkey_scan::scan_keys(&mut self.conn, &pattern).await?;

        let mut sites = HashSet::new();

        for key in keys {
            // Extract site_id from key like "{your_site}:gnode:comms:production".
            // split(":gnode:comms:").next() yields "{your_site}"; strip braces.
            if let Some(wrapped) = key.split(":gnode:comms:").next() {
                let site_id = wrapped.trim_start_matches('{').trim_end_matches('}');
                if !site_id.is_empty() {
                    sites.insert(site_id.to_string());
                }
            }
        }

        // Also check gNode site registry. an earlier hardening pass: SCAN cursor.
        let site_meta_keys = crate::valkey_scan::scan_keys(&mut self.conn, "gnode:site:*:meta")
            .await
            .unwrap_or_default();

        for key in site_meta_keys {
            // Extract site_id from "gnode:site:your_site:meta". Strip any
            // hash-tag braces: a stray "gnode:site:{your_site}:meta" (written
            // by a caller that leaked the brace-literal form) would otherwise
            // register a phantom "{your_site}" site — a second SQLite DB and a
            // duplicate discovery every restart. Mirror the stream-key path.
            let parts: Vec<&str> = key.split(':').collect();
            if parts.len() >= 3 {
                let site_id = parts[2].trim_start_matches('{').trim_end_matches('}');
                if !site_id.is_empty() {
                    sites.insert(site_id.to_string());
                }
            }
        }

        // Update known sites
        {
            let mut known = self.known_sites.write().await;
            let new_sites: Vec<_> = sites.difference(&*known).cloned().collect();
            let removed_sites: Vec<_> = known.difference(&sites).cloned().collect();

            if known.is_empty() && !new_sites.is_empty() {
                // Initial population (e.g. every restart): one summary line, not
                // one INFO per site — the list was flooding the journal.
                let mut list: Vec<&str> = new_sites.iter().map(String::as_str).collect();
                list.sort_unstable();
                info!(count = new_sites.len(), sites = %list.join(", "), "Discovered sites");
            } else {
                // Steady state: only genuinely-new sites, logged individually.
                for site in &new_sites {
                    info!(site_id = %site, "Discovered new site");
                }
            }
            for site in &removed_sites {
                warn!(site_id = %site, "Site no longer available");
            }

            *known = sites.clone();
        }

        Ok(sites.into_iter().collect())
    }

    /// Get the comms stream key for a site. Hash-tag-safe: every per-site
    /// key in the COMMS namespace uses `{site_id}:...` brace-literal form
    /// so ValKey Cluster co-locates them in the same hash-slot.
    pub fn get_stream_key(&self, site_id: &str) -> String {
        format!("{{{}}}:gnode:comms:{}", site_id, self.environment)
    }

    /// Get all known stream keys
    pub async fn get_all_stream_keys(&self) -> Vec<String> {
        let known = self.known_sites.read().await;
        known
            .iter()
            .map(|site| self.get_stream_key(site))
            .collect()
    }

    /// Check if a site exists
    pub async fn site_exists(&self, site_id: &str) -> bool {
        let known = self.known_sites.read().await;
        known.contains(site_id)
    }
}
