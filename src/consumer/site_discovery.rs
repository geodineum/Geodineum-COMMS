//! Dynamic site discovery from GSD

use redis::aio::MultiplexedConnection;
use redis::AsyncCommands;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::error::{CommsError, Result};

/// Discovers registered sites from GSD ValKey
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
        // Pattern to find comms streams
        let pattern = format!("*:gsd:comms:{}", self.environment);

        let keys: Vec<String> = redis::cmd("KEYS")
            .arg(&pattern)
            .query_async(&mut self.conn)
            .await
            .map_err(CommsError::ValKey)?;

        let mut sites = HashSet::new();

        for key in keys {
            // Extract site_id from key like "staging_nierto_com:gsd:comms:production"
            if let Some(site_id) = key.split(":gsd:comms:").next() {
                sites.insert(site_id.to_string());
            }
        }

        // Also check GSD site registry
        let site_meta_keys: Vec<String> = redis::cmd("KEYS")
            .arg("gsd:site:*:meta")
            .query_async(&mut self.conn)
            .await
            .unwrap_or_default();

        for key in site_meta_keys {
            // Extract site_id from "gsd:site:staging_nierto_com:meta"
            let parts: Vec<&str> = key.split(':').collect();
            if parts.len() >= 3 {
                sites.insert(parts[2].to_string());
            }
        }

        // Update known sites
        {
            let mut known = self.known_sites.write().await;
            let new_sites: Vec<_> = sites.difference(&*known).cloned().collect();
            let removed_sites: Vec<_> = known.difference(&sites).cloned().collect();

            for site in &new_sites {
                info!(site_id = %site, "Discovered new site");
            }
            for site in &removed_sites {
                warn!(site_id = %site, "Site no longer available");
            }

            *known = sites.clone();
        }

        Ok(sites.into_iter().collect())
    }

    /// Get the comms stream key for a site
    pub fn get_stream_key(&self, site_id: &str) -> String {
        format!("{}:gsd:comms:{}", site_id, self.environment)
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
