//! Stream consumer for GSD comms streams

use redis::aio::MultiplexedConnection;
use redis::{AsyncCommands, RedisError};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::interval;
use tracing::{debug, error, info, instrument, warn};

use crate::channels::CommsMessage;
use crate::consumer::SiteDiscovery;
use crate::error::{CommsError, Result};

/// Consumer group name for GSD-COMMS
const CONSUMER_GROUP: &str = "gsd_comms_dispatch";

/// Stream consumer that reads from multiple site comms streams
pub struct StreamConsumer {
    conn: MultiplexedConnection,
    site_discovery: SiteDiscovery,
    consumer_name: String,
    block_ms: u64,
    batch_size: usize,
    /// Tracks the last ID read for each stream
    last_ids: HashMap<String, String>,
}

impl StreamConsumer {
    pub fn new(
        conn: MultiplexedConnection,
        site_discovery: SiteDiscovery,
        consumer_name: String,
    ) -> Self {
        Self {
            conn,
            site_discovery,
            consumer_name,
            block_ms: 5000, // 5 second block
            batch_size: 100,
            last_ids: HashMap::new(),
        }
    }

    pub fn with_block_ms(mut self, ms: u64) -> Self {
        self.block_ms = ms;
        self
    }

    pub fn with_batch_size(mut self, size: usize) -> Self {
        self.batch_size = size;
        self
    }

    /// Initialize consumer groups for all known streams
    #[instrument(skip(self))]
    pub async fn initialize_groups(&mut self) -> Result<()> {
        let sites = self.site_discovery.discover_sites().await?;

        for site_id in &sites {
            let stream_key = self.site_discovery.get_stream_key(site_id);
            self.ensure_consumer_group(&stream_key).await?;
        }

        info!(
            site_count = sites.len(),
            consumer_group = CONSUMER_GROUP,
            "Initialized consumer groups"
        );

        Ok(())
    }

    /// Ensure consumer group exists for a stream
    async fn ensure_consumer_group(&mut self, stream_key: &str) -> Result<()> {
        // Try to create the consumer group
        let result: std::result::Result<(), RedisError> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(stream_key)
            .arg(CONSUMER_GROUP)
            .arg("0") // Start from beginning
            .arg("MKSTREAM") // Create stream if it doesn't exist
            .query_async(&mut self.conn)
            .await;

        match result {
            Ok(_) => {
                debug!(stream = %stream_key, "Created consumer group");
                Ok(())
            }
            Err(e) if e.to_string().contains("BUSYGROUP") => {
                // Group already exists, this is fine
                debug!(stream = %stream_key, "Consumer group already exists");
                Ok(())
            }
            Err(e) => Err(CommsError::ValKey(e)),
        }
    }

    /// Read messages from all streams
    #[instrument(skip(self))]
    pub async fn read_messages(&mut self) -> Result<Vec<(String, CommsMessage)>> {
        // Refresh site list periodically
        let stream_keys = self.site_discovery.get_all_stream_keys().await;

        if stream_keys.is_empty() {
            debug!("No streams to read from");
            return Ok(vec![]);
        }

        // Build XREADGROUP command
        // XREADGROUP GROUP group consumer [COUNT count] [BLOCK ms] STREAMS key [key ...] id [id ...]
        let mut cmd = redis::cmd("XREADGROUP");
        cmd.arg("GROUP")
            .arg(CONSUMER_GROUP)
            .arg(&self.consumer_name)
            .arg("COUNT")
            .arg(self.batch_size)
            .arg("BLOCK")
            .arg(self.block_ms)
            .arg("STREAMS");

        // Add all stream keys
        for key in &stream_keys {
            cmd.arg(key);
        }

        // Add IDs (> for new messages)
        for _ in &stream_keys {
            cmd.arg(">");
        }

        // Execute
        let result: Option<Vec<StreamReadReply>> = cmd
            .query_async(&mut self.conn)
            .await
            .map_err(CommsError::ValKey)?;

        let mut messages = Vec::new();

        if let Some(streams) = result {
            for stream in streams {
                for entry in stream.entries {
                    match self.parse_message(&stream.key, &entry) {
                        Ok(msg) => {
                            messages.push((entry.id.clone(), msg));
                        }
                        Err(e) => {
                            warn!(
                                stream = %stream.key,
                                entry_id = %entry.id,
                                error = %e,
                                "Failed to parse message"
                            );
                        }
                    }
                }
            }
        }

        debug!(message_count = messages.len(), "Read messages from streams");

        Ok(messages)
    }

    /// Parse a stream entry into a CommsMessage
    fn parse_message(&self, stream_key: &str, entry: &StreamEntry) -> Result<CommsMessage> {
        let fields = &entry.fields;

        // Extract site_id from stream key
        let site_id = stream_key
            .split(":gsd:comms:")
            .next()
            .unwrap_or("unknown")
            .to_string();

        // Build message from fields
        let id = fields.get("id").cloned().unwrap_or_else(|| entry.id.clone());

        let message_type = fields
            .get("type")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());

        let timestamp = fields
            .get("timestamp")
            .cloned()
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

        let priority: u8 = fields
            .get("priority")
            .and_then(|p| p.parse().ok())
            .unwrap_or(3);

        // Parse sender JSON
        let sender = fields
            .get("sender")
            .and_then(|s| serde_json::from_str(s).ok());

        // Parse content JSON
        let content = fields
            .get("content")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| crate::channels::MessageContent {
                subject: fields.get("subject").cloned(),
                body: fields.get("body").or(fields.get("message")).cloned(),
                attachments: HashMap::new(),
            });

        // Parse metadata JSON
        let metadata: HashMap<String, serde_json::Value> = fields
            .get("metadata")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        // Parse dispatch JSON
        let dispatch = fields
            .get("dispatch")
            .and_then(|s| serde_json::from_str(s).ok());

        Ok(CommsMessage {
            id,
            message_type,
            timestamp,
            site_id,
            priority,
            sender,
            content,
            metadata,
            dispatch,
        })
    }

    /// Acknowledge a message as processed
    pub async fn ack_message(&mut self, stream_key: &str, message_id: &str) -> Result<()> {
        redis::cmd("XACK")
            .arg(stream_key)
            .arg(CONSUMER_GROUP)
            .arg(message_id)
            .query_async(&mut self.conn)
            .await
            .map_err(CommsError::ValKey)?;

        debug!(stream = %stream_key, message_id = %message_id, "Acknowledged message");

        Ok(())
    }

    /// Claim pending messages that have been idle too long
    pub async fn claim_pending(&mut self, idle_ms: u64) -> Result<Vec<(String, CommsMessage)>> {
        let stream_keys = self.site_discovery.get_all_stream_keys().await;
        let mut claimed = Vec::new();

        for stream_key in stream_keys {
            // Use XAUTOCLAIM to claim idle messages
            let result: Option<(String, Vec<StreamEntry>, Vec<String>)> = redis::cmd("XAUTOCLAIM")
                .arg(&stream_key)
                .arg(CONSUMER_GROUP)
                .arg(&self.consumer_name)
                .arg(idle_ms)
                .arg("0-0") // Start from beginning
                .arg("COUNT")
                .arg(10)
                .query_async(&mut self.conn)
                .await
                .ok();

            if let Some((_, entries, _)) = result {
                for entry in entries {
                    if let Ok(msg) = self.parse_message(&stream_key, &entry) {
                        claimed.push((entry.id, msg));
                    }
                }
            }
        }

        if !claimed.is_empty() {
            info!(claimed_count = claimed.len(), "Claimed pending messages");
        }

        Ok(claimed)
    }
}

// Helper types for parsing XREADGROUP response
#[derive(Debug)]
struct StreamReadReply {
    key: String,
    entries: Vec<StreamEntry>,
}

#[derive(Debug)]
struct StreamEntry {
    id: String,
    fields: HashMap<String, String>,
}

// Manual implementation of FromRedisValue for our custom types
impl redis::FromRedisValue for StreamReadReply {
    fn from_redis_value(v: &redis::Value) -> redis::RedisResult<Self> {
        match v {
            redis::Value::Bulk(arr) if arr.len() >= 2 => {
                let key: String = redis::from_redis_value(&arr[0])?;
                let entries: Vec<StreamEntry> = redis::from_redis_value(&arr[1])?;
                Ok(StreamReadReply { key, entries })
            }
            _ => Err(redis::RedisError::from((
                redis::ErrorKind::TypeError,
                "Invalid stream reply format",
            ))),
        }
    }
}

impl redis::FromRedisValue for StreamEntry {
    fn from_redis_value(v: &redis::Value) -> redis::RedisResult<Self> {
        match v {
            redis::Value::Bulk(arr) if arr.len() >= 2 => {
                let id: String = redis::from_redis_value(&arr[0])?;
                let field_values: Vec<String> = redis::from_redis_value(&arr[1])?;

                let mut fields = HashMap::new();
                let mut iter = field_values.into_iter();
                while let (Some(k), Some(v)) = (iter.next(), iter.next()) {
                    fields.insert(k, v);
                }

                Ok(StreamEntry { id, fields })
            }
            _ => Err(redis::RedisError::from((
                redis::ErrorKind::TypeError,
                "Invalid entry format",
            ))),
        }
    }
}
