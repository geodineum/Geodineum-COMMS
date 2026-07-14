//! SQLite message store with per-site database isolation

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parking_lot::RwLock;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, OptionalExtension};
use tracing::{debug, info};

use crate::channels::CommsMessage;
use crate::error::{CommsError, Result};

use super::models::{ArchivedMessage, ChannelResult, ChannelStats, MessageQuery, MessageStats, MessageStatus};

/// Connection pool type
type DbPool = Pool<SqliteConnectionManager>;

/// Per-site SQLite message store
///
/// Each site gets its own SQLite database file for complete isolation.
/// Database files are stored at: {data_dir}/{site_id}/messages.db
pub struct MessageStore {
    /// Base data directory
    data_dir: PathBuf,

    /// Connection pools per site (lazy initialized)
    pools: RwLock<HashMap<String, DbPool>>,

    /// Maximum connections per pool
    max_connections: u32,
}

impl MessageStore {
    /// Create a new message store
    ///
    /// # Arguments
    /// * `data_dir` - Base directory for SQLite databases (e.g., /var/lib/geodineum-comms)
    pub fn new(data_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();

        // Ensure base directory exists
        std::fs::create_dir_all(&data_dir).map_err(|e| {
            CommsError::Internal(format!("Failed to create data directory: {}", e))
        })?;

        info!(data_dir = %data_dir.display(), "Message store initialized");

        Ok(Self {
            data_dir,
            pools: RwLock::new(HashMap::new()),
            max_connections: 5,
        })
    }

    /// Get the data directory path
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Get or create connection pool for a site
    pub(crate) fn get_pool(&self, site_id: &str) -> Result<DbPool> {
        // Check if pool exists
        {
            let pools = self.pools.read();
            if let Some(pool) = pools.get(site_id) {
                return Ok(pool.clone());
            }
        }

        // Create new pool
        let mut pools = self.pools.write();

        // Double-check after acquiring write lock
        if let Some(pool) = pools.get(site_id) {
            return Ok(pool.clone());
        }

        // Create site directory
        let site_dir = self.data_dir.join(site_id);
        std::fs::create_dir_all(&site_dir).map_err(|e| {
            CommsError::Internal(format!("Failed to create site directory: {}", e))
        })?;

        let db_path = site_dir.join("messages.db");
        debug!(site_id = site_id, path = %db_path.display(), "Opening SQLite database");

        let manager = SqliteConnectionManager::file(&db_path);
        let pool = Pool::builder()
            .max_size(self.max_connections)
            .build(manager)
            .map_err(|e| CommsError::Internal(format!("Failed to create pool: {}", e)))?;

        // Initialize schema
        self.init_schema(&pool)?;

        pools.insert(site_id.to_string(), pool.clone());
        info!(site_id = site_id, "Created new database for site");

        Ok(pool)
    }

    /// Initialize database schema
    fn init_schema(&self, pool: &DbPool) -> Result<()> {
        let conn = pool.get().map_err(|e| {
            CommsError::Internal(format!("Failed to get connection: {}", e))
        })?;

        conn.execute_batch(
            r#"
            -- Messages table
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                stream_id TEXT NOT NULL,
                site_id TEXT NOT NULL,
                environment TEXT NOT NULL DEFAULT 'production',
                message_type TEXT NOT NULL,
                priority INTEGER NOT NULL DEFAULT 3,
                sender_json TEXT NOT NULL DEFAULT '{}',
                content_json TEXT NOT NULL DEFAULT '{}',
                metadata_json TEXT NOT NULL DEFAULT '{}',
                status TEXT NOT NULL DEFAULT 'pending',
                attempts INTEGER NOT NULL DEFAULT 0,
                channel_results_json TEXT NOT NULL DEFAULT '[]',
                spam_score REAL,
                received_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                completed_at TEXT
            );

            -- Indexes for common queries
            CREATE INDEX IF NOT EXISTS idx_messages_status ON messages(status);
            CREATE INDEX IF NOT EXISTS idx_messages_type ON messages(message_type);
            CREATE INDEX IF NOT EXISTS idx_messages_received ON messages(received_at DESC);
            CREATE INDEX IF NOT EXISTS idx_messages_environment ON messages(environment);

            -- Channel results table (denormalized for quick queries)
            CREATE TABLE IF NOT EXISTS channel_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id TEXT NOT NULL,
                channel TEXT NOT NULL,
                success INTEGER NOT NULL DEFAULT 0,
                provider_id TEXT,
                error TEXT,
                sent_at TEXT,
                FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_channel_results_message ON channel_results(message_id);
            CREATE INDEX IF NOT EXISTS idx_channel_results_channel ON channel_results(channel);
            "#,
        )
        .map_err(|e| CommsError::Internal(format!("Failed to initialize schema: {}", e)))?;

        Ok(())
    }

    /// Archive a message when it arrives
    pub fn archive_received(&self, site_id: &str, message: &CommsMessage, stream_id: &str) -> Result<()> {
        let pool = self.get_pool(site_id)?;
        let conn = pool.get().map_err(|e| {
            CommsError::Internal(format!("Failed to get connection: {}", e))
        })?;

        let now = chrono::Utc::now().to_rfc3339();
        let sender_json = serde_json::to_string(&message.sender).unwrap_or_default();
        let content_json = serde_json::to_string(&message.content).unwrap_or_default();
        let metadata_json = serde_json::to_string(&message.metadata).unwrap_or_default();

        conn.execute(
            r#"
            INSERT INTO messages (
                id, stream_id, site_id, environment, message_type, priority,
                sender_json, content_json, metadata_json, status, received_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10, ?10)
            ON CONFLICT(id) DO UPDATE SET
                updated_at = ?10
            WHERE status NOT IN ('sent', 'spam', 'skipped')
            "#,
            params![
                message.id,
                stream_id,
                site_id,
                message.metadata.get("environment").and_then(|v| v.as_str()).unwrap_or("production"),
                message.message_type,
                message.priority,
                sender_json,
                content_json,
                metadata_json,
                now,
            ],
        )
        .map_err(|e| CommsError::Internal(format!("Failed to archive message: {}", e)))?;

        debug!(message_id = %message.id, site_id = site_id, "Message archived");
        Ok(())
    }

    /// Check if a message has already been successfully sent (dedup guard)
    pub fn is_already_sent(&self, site_id: &str, message_id: &str) -> bool {
        let pool = match self.get_pool(site_id) {
            Ok(p) => p,
            Err(_) => return false,
        };
        let conn = match pool.get() {
            Ok(c) => c,
            Err(_) => return false,
        };

        conn.query_row(
            "SELECT status FROM messages WHERE id = ?1",
            params![message_id],
            |row| row.get::<_, String>(0),
        )
        .map(|status| status == "sent")
        .unwrap_or(false)
    }

    /// Update message status
    pub fn update_status(&self, site_id: &str, message_id: &str, status: MessageStatus) -> Result<()> {
        let pool = self.get_pool(site_id)?;
        let conn = pool.get().map_err(|e| {
            CommsError::Internal(format!("Failed to get connection: {}", e))
        })?;

        let now = chrono::Utc::now().to_rfc3339();
        let completed_at = match status {
            MessageStatus::Sent | MessageStatus::Failed | MessageStatus::Spam | MessageStatus::Skipped => {
                Some(now.clone())
            }
            _ => None,
        };

        conn.execute(
            r#"
            UPDATE messages
            SET status = ?1, updated_at = ?2, completed_at = COALESCE(?3, completed_at)
            WHERE id = ?4
            "#,
            params![status.as_str(), now, completed_at, message_id],
        )
        .map_err(|e| CommsError::Internal(format!("Failed to update status: {}", e)))?;

        Ok(())
    }

    /// Record channel dispatch result
    pub fn record_channel_result(
        &self,
        site_id: &str,
        message_id: &str,
        result: &ChannelResult,
    ) -> Result<()> {
        let pool = self.get_pool(site_id)?;
        let conn = pool.get().map_err(|e| {
            CommsError::Internal(format!("Failed to get connection: {}", e))
        })?;

        conn.execute(
            r#"
            INSERT INTO channel_results (message_id, channel, success, provider_id, error, sent_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                message_id,
                result.channel,
                result.success as i32,
                result.provider_id,
                result.error,
                result.sent_at,
            ],
        )
        .map_err(|e| CommsError::Internal(format!("Failed to record channel result: {}", e)))?;

        // Update the JSON array in messages table
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            r#"
            UPDATE messages
            SET
                channel_results_json = (
                    SELECT json_group_array(
                        json_object(
                            'channel', channel,
                            'success', success,
                            'provider_id', provider_id,
                            'error', error,
                            'sent_at', sent_at
                        )
                    )
                    FROM channel_results
                    WHERE message_id = ?1
                ),
                attempts = attempts + 1,
                updated_at = ?2
            WHERE id = ?1
            "#,
            params![message_id, now],
        )
        .map_err(|e| CommsError::Internal(format!("Failed to update message: {}", e)))?;

        Ok(())
    }

    /// Complete message processing with final status
    pub fn complete_message(
        &self,
        site_id: &str,
        message_id: &str,
        successful_channels: &[String],
        failed_channels: &[String],
    ) -> Result<()> {
        let status = if failed_channels.is_empty() {
            MessageStatus::Sent
        } else if successful_channels.is_empty() {
            MessageStatus::Failed
        } else {
            MessageStatus::PartialSent
        };

        self.update_status(site_id, message_id, status)
    }

    /// Mark message as spam
    pub fn mark_spam(&self, site_id: &str, message_id: &str, spam_score: f64) -> Result<()> {
        let pool = self.get_pool(site_id)?;
        let conn = pool.get().map_err(|e| {
            CommsError::Internal(format!("Failed to get connection: {}", e))
        })?;

        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE messages
            SET status = 'spam', spam_score = ?1, updated_at = ?2, completed_at = ?2
            WHERE id = ?3
            "#,
            params![spam_score, now, message_id],
        )
        .map_err(|e| CommsError::Internal(format!("Failed to mark spam: {}", e)))?;

        Ok(())
    }

    /// Get a single message by ID
    pub fn get_message(&self, site_id: &str, message_id: &str) -> Result<Option<ArchivedMessage>> {
        let pool = self.get_pool(site_id)?;
        let conn = pool.get().map_err(|e| {
            CommsError::Internal(format!("Failed to get connection: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, stream_id, site_id, environment, message_type, priority,
                       sender_json, content_json, metadata_json, status, attempts,
                       channel_results_json, spam_score, received_at, updated_at, completed_at
                FROM messages
                WHERE id = ?1
                "#,
            )
            .map_err(|e| CommsError::Internal(format!("Failed to prepare query: {}", e)))?;

        let result = stmt
            .query_row(params![message_id], |row| {
                Ok(ArchivedMessage {
                    id: row.get(0)?,
                    stream_id: row.get(1)?,
                    site_id: row.get(2)?,
                    environment: row.get(3)?,
                    message_type: row.get(4)?,
                    priority: row.get(5)?,
                    sender_json: row.get(6)?,
                    content_json: row.get(7)?,
                    metadata_json: row.get(8)?,
                    status: MessageStatus::from_str(&row.get::<_, String>(9)?),
                    attempts: row.get(10)?,
                    channel_results_json: row.get(11)?,
                    spam_score: row.get(12)?,
                    received_at: row.get(13)?,
                    updated_at: row.get(14)?,
                    completed_at: row.get(15)?,
                })
            })
            .optional()
            .map_err(|e| CommsError::Internal(format!("Failed to query message: {}", e)))?;

        Ok(result)
    }

    /// List messages with filtering
    pub fn list_messages(&self, site_id: &str, query: &MessageQuery) -> Result<Vec<ArchivedMessage>> {
        let pool = self.get_pool(site_id)?;
        let conn = pool.get().map_err(|e| {
            CommsError::Internal(format!("Failed to get connection: {}", e))
        })?;

        let mut sql = String::from(
            r#"
            SELECT id, stream_id, site_id, environment, message_type, priority,
                   sender_json, content_json, metadata_json, status, attempts,
                   channel_results_json, spam_score, received_at, updated_at, completed_at
            FROM messages
            WHERE 1=1
            "#,
        );

        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(status) = &query.status {
            sql.push_str(" AND status = ?");
            params_vec.push(Box::new(status.as_str().to_string()));
        }

        if let Some(msg_type) = &query.message_type {
            sql.push_str(" AND message_type = ?");
            params_vec.push(Box::new(msg_type.clone()));
        }

        if let Some(since) = &query.since {
            sql.push_str(" AND received_at >= ?");
            params_vec.push(Box::new(since.clone()));
        }

        if let Some(until) = &query.until {
            sql.push_str(" AND received_at <= ?");
            params_vec.push(Box::new(until.clone()));
        }

        sql.push_str(" ORDER BY received_at DESC");
        sql.push_str(&format!(" LIMIT {} OFFSET {}", query.limit.max(1).min(1000), query.offset));

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| CommsError::Internal(format!("Failed to prepare query: {}", e)))?;

        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

        let rows = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok(ArchivedMessage {
                    id: row.get(0)?,
                    stream_id: row.get(1)?,
                    site_id: row.get(2)?,
                    environment: row.get(3)?,
                    message_type: row.get(4)?,
                    priority: row.get(5)?,
                    sender_json: row.get(6)?,
                    content_json: row.get(7)?,
                    metadata_json: row.get(8)?,
                    status: MessageStatus::from_str(&row.get::<_, String>(9)?),
                    attempts: row.get(10)?,
                    channel_results_json: row.get(11)?,
                    spam_score: row.get(12)?,
                    received_at: row.get(13)?,
                    updated_at: row.get(14)?,
                    completed_at: row.get(15)?,
                })
            })
            .map_err(|e| CommsError::Internal(format!("Failed to query messages: {}", e)))?;

        let messages: std::result::Result<Vec<_>, _> = rows.collect();
        messages.map_err(|e| CommsError::Internal(format!("Failed to collect messages: {}", e)))
    }

    /// Get statistics for a site
    pub fn get_stats(&self, site_id: &str) -> Result<MessageStats> {
        let pool = self.get_pool(site_id)?;
        let conn = pool.get().map_err(|e| {
            CommsError::Internal(format!("Failed to get connection: {}", e))
        })?;

        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let week_ago = (chrono::Utc::now() - chrono::Duration::days(7))
            .format("%Y-%m-%d")
            .to_string();

        let row: (i64, i64, i64, i64, i64, i64, i64, i64) = conn
            .query_row(
                r#"
                SELECT
                    COUNT(*) as total,
                    SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END) as pending,
                    SUM(CASE WHEN status = 'sent' THEN 1 ELSE 0 END) as sent,
                    SUM(CASE WHEN status = 'partial_sent' THEN 1 ELSE 0 END) as partial_sent,
                    SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) as failed,
                    SUM(CASE WHEN status = 'spam' THEN 1 ELSE 0 END) as spam,
                    SUM(CASE WHEN status IN ('sent', 'partial_sent') AND DATE(received_at) = ?1 THEN 1 ELSE 0 END) as sent_today,
                    SUM(CASE WHEN status IN ('sent', 'partial_sent') AND DATE(received_at) >= ?2 THEN 1 ELSE 0 END) as sent_week
                FROM messages
                "#,
                params![today, week_ago],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .map_err(|e| CommsError::Internal(format!("Failed to get stats: {}", e)))?;

        // Get channel stats
        let channel_stats = conn
            .query_row(
                r#"
                SELECT
                    SUM(CASE WHEN channel = 'email' AND success = 1 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN channel = 'email' AND success = 0 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN channel = 'telegram' AND success = 1 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN channel = 'telegram' AND success = 0 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN channel = 'sms' AND success = 1 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN channel = 'sms' AND success = 0 THEN 1 ELSE 0 END)
                FROM channel_results
                "#,
                [],
                |row| {
                    Ok(ChannelStats {
                        email_sent: row.get::<_, Option<i64>>(0)?.unwrap_or(0) as u64,
                        email_failed: row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
                        telegram_sent: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                        telegram_failed: row.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
                        sms_sent: row.get::<_, Option<i64>>(4)?.unwrap_or(0) as u64,
                        sms_failed: row.get::<_, Option<i64>>(5)?.unwrap_or(0) as u64,
                    })
                },
            )
            .unwrap_or_default();

        Ok(MessageStats {
            total: row.0 as u64,
            pending: row.1 as u64,
            sent: row.2 as u64,
            partial_sent: row.3 as u64,
            failed: row.4 as u64,
            spam: row.5 as u64,
            sent_today: row.6 as u64,
            sent_this_week: row.7 as u64,
            by_channel: channel_stats,
        })
    }

    /// List all sites with databases
    pub fn list_sites(&self) -> Result<Vec<String>> {
        let mut sites = Vec::new();

        for entry in std::fs::read_dir(&self.data_dir).map_err(|e| {
            CommsError::Internal(format!("Failed to read data directory: {}", e))
        })? {
            let entry = entry.map_err(|e| {
                CommsError::Internal(format!("Failed to read entry: {}", e))
            })?;

            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    // Check if it has a messages.db
                    let db_path = entry.path().join("messages.db");
                    if db_path.exists() {
                        sites.push(name.to_string());
                    }
                }
            }
        }

        Ok(sites)
    }
}
