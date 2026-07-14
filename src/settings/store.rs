//! Settings storage in ValKey

use parking_lot::RwLock;
use redis::aio::MultiplexedConnection;
use redis::AsyncCommands;
use std::collections::HashMap;
use tracing::{debug, info, warn};

use crate::error::{CommsError, Result};
use crate::settings::SiteSettings;

/// ValKey-backed settings store with in-memory caching
pub struct SettingsStore {
    conn: MultiplexedConnection,
    cache: RwLock<HashMap<String, SiteSettings>>,
}

impl SettingsStore {
    pub fn new(conn: MultiplexedConnection) -> Self {
        Self {
            conn,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Get the ValKey key for site settings
    fn settings_key(site_id: &str) -> String {
        format!("{{{}}}:comms:config", site_id)
    }

    /// Get settings for a site
    pub async fn get_settings(&self, site_id: &str) -> Result<Option<SiteSettings>> {
        // Check cache first
        {
            let cache = self.cache.read();
            if let Some(settings) = cache.get(site_id) {
                return Ok(Some(settings.clone()));
            }
        }

        // Load from ValKey
        let key = Self::settings_key(site_id);
        let mut conn = self.conn.clone();

        let data: Option<String> = conn.get(&key).await.map_err(CommsError::ValKey)?;

        match data {
            Some(json) => {
                let settings: SiteSettings = serde_json::from_str(&json).map_err(|e| {
                    CommsError::Config(format!("Invalid settings JSON for {}: {}", site_id, e))
                })?;

                // Update cache
                {
                    let mut cache = self.cache.write();
                    cache.insert(site_id.to_string(), settings.clone());
                }

                Ok(Some(settings))
            }
            None => {
                debug!(site_id = %site_id, "No settings found");
                Ok(None)
            }
        }
    }

    /// Save settings for a site
    pub async fn save_settings(&self, settings: &SiteSettings) -> Result<()> {
        let key = Self::settings_key(&settings.site_id);
        let json = serde_json::to_string_pretty(settings)?;

        let mut conn = self.conn.clone();
        conn.set::<_, _, ()>(&key, &json)
            .await
            .map_err(CommsError::ValKey)?;

        // Update cache
        {
            let mut cache = self.cache.write();
            cache.insert(settings.site_id.clone(), settings.clone());
        }

        info!(site_id = %settings.site_id, "Saved site settings");

        Ok(())
    }

    /// Delete settings for a site
    pub async fn delete_settings(&self, site_id: &str) -> Result<()> {
        let key = Self::settings_key(site_id);
        let mut conn = self.conn.clone();

        conn.del::<_, ()>(&key)
            .await
            .map_err(CommsError::ValKey)?;

        // Remove from cache
        {
            let mut cache = self.cache.write();
            cache.remove(site_id);
        }

        info!(site_id = %site_id, "Deleted site settings");

        Ok(())
    }

    /// List all sites with settings
    pub async fn list_sites(&self) -> Result<Vec<String>> {
        let mut conn = self.conn.clone();

        // an earlier hardening pass: SCAN cursor instead of blocking KEYS.
        let keys = crate::valkey_scan::scan_keys(&mut conn, "*:comms:config").await?;

        // Keys are brace-literal hash-tags ("{site}:comms:config");
        // braces are key syntax, never part of the site_id. Legacy
        // unbraced keys (pre-migration) strip to the same id.
        let sites: Vec<String> = keys
            .iter()
            .filter_map(|k| {
                k.strip_suffix(":comms:config")
                    .map(|s| s.trim_start_matches('{').trim_end_matches('}').to_string())
            })
            .collect();

        Ok(sites)
    }

    /// Invalidate cache for a site
    pub fn invalidate_cache(&self, site_id: &str) {
        let mut cache = self.cache.write();
        cache.remove(site_id);
    }

    /// Clear entire cache
    pub fn clear_cache(&self) {
        let mut cache = self.cache.write();
        cache.clear();
    }

    /// Initialize default settings for a site if none exist
    pub async fn ensure_settings(&self, site_id: &str) -> Result<SiteSettings> {
        if let Some(settings) = self.get_settings(site_id).await? {
            return Ok(settings);
        }

        // Create default settings
        let settings = SiteSettings::new(site_id);
        self.save_settings(&settings).await?;

        warn!(
            site_id = %site_id,
            "Created default settings (notifications disabled by default)"
        );

        Ok(settings)
    }
}
