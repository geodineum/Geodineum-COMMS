//! Database cleanup and maintenance
//!
//! Handles automatic deletion of old messages based on retention policies.

use std::time::Instant;

use rusqlite::params;
use tracing::{debug, info, warn};

use super::retention::{CleanupResult, RetentionConfig, SpamRetentionPolicy};
use super::store::MessageStore;
use crate::error::{CommsError, Result};

impl MessageStore {
    /// Run cleanup for all sites based on retention config
    pub fn cleanup_all(&self, config: &RetentionConfig) -> Result<Vec<(String, CleanupResult)>> {
        if !config.enabled {
            return Ok(vec![]);
        }

        let sites = self.list_sites()?;
        let mut results = Vec::new();

        for site_id in sites {
            match self.cleanup_site(&site_id, config) {
                Ok(result) => {
                    if result.had_cleanup() {
                        info!(
                            site_id = %site_id,
                            deleted = result.total_deleted,
                            space_reclaimed_mb = result.space_reclaimed() / (1024 * 1024),
                            "Site cleanup completed"
                        );
                    }
                    results.push((site_id, result));
                }
                Err(e) => {
                    warn!(site_id = %site_id, error = %e, "Cleanup failed for site");
                    let mut result = CleanupResult::default();
                    result.errors.push(e.to_string());
                    results.push((site_id, result));
                }
            }
        }

        Ok(results)
    }

    /// Run cleanup for a single site
    pub fn cleanup_site(&self, site_id: &str, config: &RetentionConfig) -> Result<CleanupResult> {
        let start = Instant::now();
        let pool = self.get_pool(site_id)?;
        let conn = pool.get().map_err(|e| {
            CommsError::Internal(format!("Failed to get connection: {}", e))
        })?;

        let mut result = CleanupResult::default();

        // Get initial database size
        result.size_before = self.get_db_size(site_id).unwrap_or(0);

        // 1. Delete by age
        if config.max_age_days > 0 {
            let deleted = self.delete_by_age(&conn, config)?;
            result.deleted_by_age = deleted;
            result.total_deleted += deleted;
        }

        // 2. Delete spam based on policy
        let spam_deleted = self.delete_spam(&conn, &config.spam_policy)?;
        result.deleted_spam = spam_deleted;
        result.total_deleted += spam_deleted;

        // 3. Delete by count limit
        if config.max_messages_per_site > 0 {
            let deleted = self.delete_by_count(&conn, config.max_messages_per_site, config.min_messages_keep)?;
            result.deleted_by_count = deleted;
            result.total_deleted += deleted;
        }

        // 4. Delete by size limit
        if config.max_db_size_mb > 0 {
            let max_bytes = config.max_db_size_mb * 1024 * 1024;
            let deleted = self.delete_by_size(&conn, site_id, max_bytes, config.min_messages_keep)?;
            result.deleted_by_size = deleted;
            result.total_deleted += deleted;
        }

        // 5. Vacuum if requested and we deleted something
        if config.vacuum_after_cleanup && result.total_deleted > 0 {
            match conn.execute("VACUUM", []) {
                Ok(_) => {
                    result.vacuumed = true;
                    debug!(site_id = %site_id, "Database vacuumed");
                }
                Err(e) => {
                    result.errors.push(format!("Vacuum failed: {}", e));
                }
            }
        }

        // Get final database size
        result.size_after = self.get_db_size(site_id).unwrap_or(result.size_before);

        debug!(
            site_id = %site_id,
            elapsed_ms = start.elapsed().as_millis(),
            total_deleted = result.total_deleted,
            "Cleanup completed"
        );

        Ok(result)
    }

    /// Delete messages older than the configured age
    fn delete_by_age(&self, conn: &rusqlite::Connection, config: &RetentionConfig) -> Result<u64> {
        let mut total_deleted = 0u64;

        // Delete sent messages by age
        let sent_age = config.effective_sent_max_age();
        if sent_age > 0 {
            let cutoff = chrono::Utc::now() - chrono::Duration::days(sent_age as i64);
            let cutoff_str = cutoff.to_rfc3339();

            let deleted = conn.execute(
                "DELETE FROM messages WHERE status = 'sent' AND received_at < ?1",
                params![cutoff_str],
            ).map_err(|e| CommsError::Internal(format!("Delete by age failed: {}", e)))?;

            total_deleted += deleted as u64;
        }

        // Delete failed messages by age
        let failed_age = config.effective_failed_max_age();
        if failed_age > 0 {
            let cutoff = chrono::Utc::now() - chrono::Duration::days(failed_age as i64);
            let cutoff_str = cutoff.to_rfc3339();

            let deleted = conn.execute(
                "DELETE FROM messages WHERE status IN ('failed', 'partial_sent') AND received_at < ?1",
                params![cutoff_str],
            ).map_err(|e| CommsError::Internal(format!("Delete by age failed: {}", e)))?;

            total_deleted += deleted as u64;
        }

        // Delete other messages by general max age
        if config.max_age_days > 0 {
            let cutoff = chrono::Utc::now() - chrono::Duration::days(config.max_age_days as i64);
            let cutoff_str = cutoff.to_rfc3339();

            let deleted = conn.execute(
                "DELETE FROM messages WHERE status NOT IN ('sent', 'failed', 'partial_sent', 'spam') AND received_at < ?1",
                params![cutoff_str],
            ).map_err(|e| CommsError::Internal(format!("Delete by age failed: {}", e)))?;

            total_deleted += deleted as u64;
        }

        Ok(total_deleted)
    }

    /// Delete spam messages based on policy
    fn delete_spam(&self, conn: &rusqlite::Connection, policy: &SpamRetentionPolicy) -> Result<u64> {
        match policy {
            SpamRetentionPolicy::Keep => Ok(0),
            SpamRetentionPolicy::NeverDelete => Ok(0),
            SpamRetentionPolicy::DeleteImmediately => {
                let deleted = conn.execute(
                    "DELETE FROM messages WHERE status = 'spam'",
                    [],
                ).map_err(|e| CommsError::Internal(format!("Delete spam failed: {}", e)))?;
                Ok(deleted as u64)
            }
            SpamRetentionPolicy::DeleteAfterDays(days) => {
                let cutoff = chrono::Utc::now() - chrono::Duration::days(*days as i64);
                let cutoff_str = cutoff.to_rfc3339();

                let deleted = conn.execute(
                    "DELETE FROM messages WHERE status = 'spam' AND received_at < ?1",
                    params![cutoff_str],
                ).map_err(|e| CommsError::Internal(format!("Delete spam failed: {}", e)))?;
                Ok(deleted as u64)
            }
        }
    }

    /// Delete oldest messages when count exceeds limit
    fn delete_by_count(&self, conn: &rusqlite::Connection, max_count: u64, min_keep: u64) -> Result<u64> {
        // Get current count
        let current_count: u64 = conn.query_row(
            "SELECT COUNT(*) FROM messages",
            [],
            |row| row.get(0),
        ).map_err(|e| CommsError::Internal(format!("Count query failed: {}", e)))?;

        if current_count <= max_count {
            return Ok(0);
        }

        // Calculate how many to delete (respect min_keep)
        let effective_max = max_count.max(min_keep);
        if current_count <= effective_max {
            return Ok(0);
        }

        let to_delete = current_count - effective_max;

        // Delete oldest messages (preserve pending/processing)
        let deleted = conn.execute(
            r#"
            DELETE FROM messages WHERE id IN (
                SELECT id FROM messages
                WHERE status NOT IN ('pending', 'processing')
                ORDER BY received_at ASC
                LIMIT ?1
            )
            "#,
            params![to_delete],
        ).map_err(|e| CommsError::Internal(format!("Delete by count failed: {}", e)))?;

        Ok(deleted as u64)
    }

    /// Delete oldest messages when database exceeds size limit
    fn delete_by_size(&self, conn: &rusqlite::Connection, site_id: &str, max_bytes: u64, min_keep: u64) -> Result<u64> {
        let current_size = self.get_db_size(site_id).unwrap_or(0);

        if current_size <= max_bytes {
            return Ok(0);
        }

        // Estimate messages to delete based on average message size
        let (count, total_size): (u64, u64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(sender_json) + LENGTH(content_json) + LENGTH(metadata_json)), 0) FROM messages",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).map_err(|e| CommsError::Internal(format!("Size query failed: {}", e)))?;

        if count == 0 {
            return Ok(0);
        }

        let avg_size = total_size / count;
        let excess = current_size - max_bytes;
        let estimated_delete = (excess / avg_size.max(1)).max(1);

        // Respect min_keep
        let max_deletable = count.saturating_sub(min_keep);
        let to_delete = estimated_delete.min(max_deletable);

        if to_delete == 0 {
            return Ok(0);
        }

        // Delete oldest non-active messages
        let deleted = conn.execute(
            r#"
            DELETE FROM messages WHERE id IN (
                SELECT id FROM messages
                WHERE status NOT IN ('pending', 'processing')
                ORDER BY received_at ASC
                LIMIT ?1
            )
            "#,
            params![to_delete],
        ).map_err(|e| CommsError::Internal(format!("Delete by size failed: {}", e)))?;

        Ok(deleted as u64)
    }

    /// Get database file size in bytes
    fn get_db_size(&self, site_id: &str) -> Option<u64> {
        let db_path = self.data_dir().join(site_id).join("messages.db");
        std::fs::metadata(&db_path).ok().map(|m| m.len())
    }

    /// Get database statistics for a site
    pub fn get_db_stats(&self, site_id: &str) -> Result<DbStats> {
        let pool = self.get_pool(site_id)?;
        let conn = pool.get().map_err(|e| {
            CommsError::Internal(format!("Failed to get connection: {}", e))
        })?;

        let message_count: u64 = conn.query_row(
            "SELECT COUNT(*) FROM messages",
            [],
            |row| row.get(0),
        ).map_err(|e| CommsError::Internal(format!("Count query failed: {}", e)))?;

        let spam_count: u64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE status = 'spam'",
            [],
            |row| row.get(0),
        ).map_err(|e| CommsError::Internal(format!("Spam count query failed: {}", e)))?;

        let oldest_message: Option<String> = conn.query_row(
            "SELECT MIN(received_at) FROM messages",
            [],
            |row| row.get(0),
        ).ok();

        let newest_message: Option<String> = conn.query_row(
            "SELECT MAX(received_at) FROM messages",
            [],
            |row| row.get(0),
        ).ok();

        Ok(DbStats {
            site_id: site_id.to_string(),
            file_size_bytes: self.get_db_size(site_id).unwrap_or(0),
            message_count,
            spam_count,
            oldest_message,
            newest_message,
        })
    }
}

/// Database statistics for a site
#[derive(Debug, Clone)]
pub struct DbStats {
    pub site_id: String,
    pub file_size_bytes: u64,
    pub message_count: u64,
    pub spam_count: u64,
    pub oldest_message: Option<String>,
    pub newest_message: Option<String>,
}

impl DbStats {
    /// Get file size in human-readable format
    pub fn file_size_human(&self) -> String {
        let bytes = self.file_size_bytes;
        if bytes < 1024 {
            format!("{} B", bytes)
        } else if bytes < 1024 * 1024 {
            format!("{:.1} KB", bytes as f64 / 1024.0)
        } else if bytes < 1024 * 1024 * 1024 {
            format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
        } else {
            format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_stats_size_human() {
        let stats = DbStats {
            site_id: "test".to_string(),
            file_size_bytes: 1500,
            message_count: 0,
            spam_count: 0,
            oldest_message: None,
            newest_message: None,
        };
        assert!(stats.file_size_human().contains("KB"));

        let stats2 = DbStats {
            file_size_bytes: 2 * 1024 * 1024,
            ..stats.clone()
        };
        assert!(stats2.file_size_human().contains("MB"));
    }
}
