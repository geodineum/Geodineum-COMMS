//! Message retention and cleanup configuration
//!
//! Configures automatic deletion of old messages, spam cleanup, and database size limits.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Retention policy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// Whether retention policies are enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Maximum number of messages to keep per site (0 = unlimited)
    #[serde(default)]
    pub max_messages_per_site: u64,

    /// Maximum database size in MB per site (0 = unlimited)
    #[serde(default)]
    pub max_db_size_mb: u64,

    /// Delete messages older than this many days (0 = never)
    #[serde(default = "default_max_age_days")]
    pub max_age_days: u32,

    /// Delete sent messages older than this many days (0 = use max_age_days)
    #[serde(default)]
    pub sent_max_age_days: u32,

    /// Delete failed messages older than this many days (0 = use max_age_days)
    #[serde(default)]
    pub failed_max_age_days: u32,

    /// How to handle spam messages
    #[serde(default)]
    pub spam_policy: SpamRetentionPolicy,

    /// How often to run cleanup (in seconds)
    #[serde(default = "default_cleanup_interval")]
    pub cleanup_interval_secs: u64,

    /// Whether to vacuum database after cleanup (reclaims disk space)
    #[serde(default = "default_vacuum")]
    pub vacuum_after_cleanup: bool,

    /// Minimum messages to keep even when over limits (safety buffer)
    #[serde(default = "default_min_keep")]
    pub min_messages_keep: u64,
}

fn default_enabled() -> bool {
    true
}

fn default_max_age_days() -> u32 {
    90 // 3 months default
}

fn default_cleanup_interval() -> u64 {
    3600 // 1 hour
}

fn default_vacuum() -> bool {
    true
}

fn default_min_keep() -> u64 {
    100 // Always keep at least 100 messages
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_messages_per_site: 0,        // Unlimited by default
            max_db_size_mb: 0,               // Unlimited by default
            max_age_days: 90,                // 3 months
            sent_max_age_days: 0,            // Use max_age_days
            failed_max_age_days: 0,          // Use max_age_days
            spam_policy: SpamRetentionPolicy::default(),
            cleanup_interval_secs: 3600,     // 1 hour
            vacuum_after_cleanup: true,
            min_messages_keep: 100,
        }
    }
}

impl RetentionConfig {
    /// Get the effective max age for sent messages
    pub fn effective_sent_max_age(&self) -> u32 {
        if self.sent_max_age_days > 0 {
            self.sent_max_age_days
        } else {
            self.max_age_days
        }
    }

    /// Get the effective max age for failed messages
    pub fn effective_failed_max_age(&self) -> u32 {
        if self.failed_max_age_days > 0 {
            self.failed_max_age_days
        } else {
            self.max_age_days
        }
    }

    /// Get cleanup interval as Duration
    pub fn cleanup_interval(&self) -> Duration {
        Duration::from_secs(self.cleanup_interval_secs)
    }
}

/// Policy for handling spam messages
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SpamRetentionPolicy {
    /// Keep spam messages like normal messages (use max_age_days)
    Keep,
    /// Delete spam immediately after processing
    DeleteImmediately,
    /// Delete spam after N days (for review period)
    DeleteAfterDays(u32),
    /// Never delete spam (for analysis)
    NeverDelete,
}

impl Default for SpamRetentionPolicy {
    fn default() -> Self {
        // Default: delete spam after 7 days (gives time to review false positives)
        Self::DeleteAfterDays(7)
    }
}

/// Result of a cleanup operation
#[derive(Debug, Clone, Default)]
pub struct CleanupResult {
    /// Number of messages deleted due to age
    pub deleted_by_age: u64,
    /// Number of spam messages deleted
    pub deleted_spam: u64,
    /// Number of messages deleted due to count limit
    pub deleted_by_count: u64,
    /// Number of messages deleted due to size limit
    pub deleted_by_size: u64,
    /// Total messages deleted
    pub total_deleted: u64,
    /// Whether vacuum was run
    pub vacuumed: bool,
    /// Database size before cleanup (bytes)
    pub size_before: u64,
    /// Database size after cleanup (bytes)
    pub size_after: u64,
    /// Any errors encountered (non-fatal)
    pub errors: Vec<String>,
}

impl CleanupResult {
    /// Check if any cleanup was performed
    pub fn had_cleanup(&self) -> bool {
        self.total_deleted > 0 || self.vacuumed
    }

    /// Get space reclaimed in bytes
    pub fn space_reclaimed(&self) -> u64 {
        self.size_before.saturating_sub(self.size_after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = RetentionConfig::default();
        assert!(config.enabled);
        assert_eq!(config.max_age_days, 90);
        assert_eq!(config.spam_policy, SpamRetentionPolicy::DeleteAfterDays(7));
    }

    #[test]
    fn test_effective_ages() {
        let mut config = RetentionConfig::default();
        config.max_age_days = 30;
        config.sent_max_age_days = 0;
        config.failed_max_age_days = 7;

        assert_eq!(config.effective_sent_max_age(), 30);
        assert_eq!(config.effective_failed_max_age(), 7);
    }

    #[test]
    fn test_spam_policy_serialization() {
        let policy = SpamRetentionPolicy::DeleteAfterDays(14);
        let json = serde_json::to_string(&policy).unwrap();
        assert!(json.contains("14"));
    }
}
