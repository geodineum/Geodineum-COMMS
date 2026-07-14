//! Retry manager with exponential backoff

use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use redis::aio::MultiplexedConnection;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::error::{CommsError, Result};
use crate::settings::RetrySettings;

/// State of a retryable message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryState {
    pub message_id: String,
    pub site_id: String,
    pub stream_key: String,
    pub stream_entry_id: String,
    pub attempts: u32,
    pub last_attempt: DateTime<Utc>,
    pub next_retry: DateTime<Utc>,
    pub last_error: Option<String>,
    pub failed_channels: Vec<String>,
}

/// Manages retry logic for failed messages
pub struct RetryManager {
    conn: MultiplexedConnection,
    default_settings: RetrySettings,
}

impl RetryManager {
    pub fn new(conn: MultiplexedConnection) -> Self {
        Self {
            conn,
            default_settings: RetrySettings::default(),
        }
    }

    pub fn with_settings(mut self, settings: RetrySettings) -> Self {
        self.default_settings = settings;
        self
    }

    /// Get the ValKey key for retry state
    fn retry_key(site_id: &str, message_id: &str) -> String {
        format!("{{{}}}:comms:retry:{}", site_id, message_id)
    }

    /// Calculate next retry time using exponential backoff with jitter
    pub fn calculate_next_retry(&self, attempts: u32, settings: &RetrySettings) -> DateTime<Utc> {
        // Exponential backoff: base * 2^attempts
        let delay_secs = settings.base_delay_secs * 2u64.pow(attempts.min(10));

        // Cap at max delay
        let delay_secs = delay_secs.min(settings.max_delay_secs);

        // Add jitter (±20%)
        let jitter_range = (delay_secs as f64 * 0.2) as i64;
        let jitter: i64 = if jitter_range > 0 {
            rand::thread_rng().gen_range(-jitter_range..=jitter_range)
        } else {
            0
        };

        let final_delay = (delay_secs as i64 + jitter).max(1) as i64;

        Utc::now() + Duration::seconds(final_delay)
    }

    /// Record a failed dispatch attempt
    pub async fn record_failure(
        &self,
        message_id: &str,
        site_id: &str,
        stream_key: &str,
        stream_entry_id: &str,
        error: &str,
        failed_channels: Vec<String>,
        settings: Option<&RetrySettings>,
    ) -> Result<Option<RetryState>> {
        let settings = settings.unwrap_or(&self.default_settings);
        let key = Self::retry_key(site_id, message_id);

        // Load existing state or create new
        let mut state = self.get_retry_state(site_id, message_id).await?.unwrap_or(
            RetryState {
                message_id: message_id.to_string(),
                site_id: site_id.to_string(),
                stream_key: stream_key.to_string(),
                stream_entry_id: stream_entry_id.to_string(),
                attempts: 0,
                last_attempt: Utc::now(),
                next_retry: Utc::now(),
                last_error: None,
                failed_channels: vec![],
            },
        );

        state.attempts += 1;
        state.last_attempt = Utc::now();
        state.last_error = Some(error.to_string());
        state.failed_channels = failed_channels;

        // Check if max attempts exceeded
        if state.attempts >= settings.max_attempts {
            warn!(
                message_id = %message_id,
                attempts = state.attempts,
                "Max retry attempts exceeded, giving up"
            );
            // Clean up retry state
            self.clear_retry_state(site_id, message_id).await?;
            return Ok(None);
        }

        // Calculate next retry time
        state.next_retry = self.calculate_next_retry(state.attempts, settings);

        // Save state
        let json = serde_json::to_string(&state)?;
        let mut conn = self.conn.clone();
        conn.set::<_, _, ()>(&key, &json)
            .await
            .map_err(CommsError::ValKey)?;

        // Set expiry (max_delay * 2 to ensure cleanup)
        conn.expire::<_, ()>(&key, (settings.max_delay_secs * 2) as i64)
            .await
            .ok();

        debug!(
            message_id = %message_id,
            attempt = state.attempts,
            next_retry = %state.next_retry,
            "Scheduled retry"
        );

        Ok(Some(state))
    }

    /// Get retry state for a message
    pub async fn get_retry_state(
        &self,
        site_id: &str,
        message_id: &str,
    ) -> Result<Option<RetryState>> {
        let key = Self::retry_key(site_id, message_id);
        let mut conn = self.conn.clone();

        let data: Option<String> = conn.get(&key).await.map_err(CommsError::ValKey)?;

        match data {
            Some(json) => {
                let state: RetryState = serde_json::from_str(&json)?;
                Ok(Some(state))
            }
            None => Ok(None),
        }
    }

    /// Clear retry state after successful dispatch
    pub async fn clear_retry_state(&self, site_id: &str, message_id: &str) -> Result<()> {
        let key = Self::retry_key(site_id, message_id);
        let mut conn = self.conn.clone();

        conn.del::<_, ()>(&key)
            .await
            .map_err(CommsError::ValKey)?;

        debug!(message_id = %message_id, "Cleared retry state");

        Ok(())
    }

    /// Get all messages due for retry
    pub async fn get_due_retries(&self) -> Result<Vec<RetryState>> {
        let mut conn = self.conn.clone();

        // an earlier hardening pass: SCAN cursor instead of blocking KEYS — this runs on
        // every retry tick, KEYS at scale would lock the main thread.
        let keys = crate::valkey_scan::scan_keys(&mut conn, "*:comms:retry:*").await?;

        let now = Utc::now();
        let mut due = Vec::new();

        for key in keys {
            if let Ok(Some(json)) = conn.get::<_, Option<String>>(&key).await {
                if let Ok(state) = serde_json::from_str::<RetryState>(&json) {
                    if state.next_retry <= now {
                        due.push(state);
                    }
                }
            }
        }

        if !due.is_empty() {
            info!(count = due.len(), "Found messages due for retry");
        }

        Ok(due)
    }

    /// Record successful dispatch (clears retry state if exists)
    pub async fn record_success(&self, site_id: &str, message_id: &str) -> Result<()> {
        self.clear_retry_state(site_id, message_id).await
    }
}
