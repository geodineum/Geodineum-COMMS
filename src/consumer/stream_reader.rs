//! Stream consumer for gNode comms streams

use redis::aio::MultiplexedConnection;
use redis::RedisError;
use std::collections::HashMap;
use tracing::{debug, info, instrument, warn};

use crate::channels::CommsMessage;
use crate::consumer::SiteDiscovery;
use crate::error::{CommsError, Result};

/// Consumer group name for Geodineum-COMMS
const CONSUMER_GROUP: &str = "geodineum_comms_dispatch";

// per-field byte cap for tenant-submitted JSON
// blobs arriving on ValKey `{site}:gnode:comms:*` streams. 100 MB metadata
// loads wholesale into RAM without this cap. serde_json's default recursion
// limit (128) covers depth; 64 KiB covers size. Caller gets Option<T> so
// existing .unwrap_or / .unwrap_or_default idioms are preserved.
const MAX_FIELD_BYTES: usize = 64 * 1024;

fn parse_field_safely<T: serde::de::DeserializeOwned>(field_name: &str, s: &str) -> Option<T> {
    if s.len() > MAX_FIELD_BYTES {
        warn!(
            field = field_name,
            bytes = s.len(),
            cap = MAX_FIELD_BYTES,
            "Stream field exceeds per-message byte cap; dropping"
        );
        return None;
    }
    match serde_json::from_str(s) {
        Ok(v) => Some(v),
        Err(e) => {
            debug!(field = field_name, error = %e, "Failed to parse stream field");
            None
        }
    }
}

// site_id path-traversal + injection defence.
// Stream names in ValKey are free-form; `{site_id}:gnode:comms:*` is a
// convention, not a constraint. An attacker who can XADD could stream-name
// `../../etc` and make `data_dir.join(site_id)` escape the data root
// (persistence/store.rs:82). Brace-literal hash-tags are stripped upstream
// in parse_message; post-strip the site_id MUST match the canonical
// `[a-z0-9_-]+` shape before it reaches `PathBuf::join` or cache keys.
static SITE_ID_RE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"^[a-z0-9_-]{1,64}$")
            .expect("SITE_ID_RE compile failed — regex is a compile-time literal")
    });

fn validate_site_id(site_id: &str) -> Result<()> {
    if SITE_ID_RE.is_match(site_id) {
        Ok(())
    } else {
        Err(CommsError::Internal(format!(
            "rejected site_id {:?}: does not match ^[a-z0-9_-]{{1,64}}$",
            site_id
        )))
    }
}

/// Stream consumer that reads from multiple site comms streams
pub struct StreamConsumer {
    conn: MultiplexedConnection,
    site_discovery: SiteDiscovery,
    consumer_name: String,
    block_ms: u64,
    batch_size: usize,
    /// Tracks the last ID read for each stream.
    /// Populated by stream-read paths; reserved for resume-on-restart wiring.
    #[allow(dead_code)]
    last_ids: HashMap<String, String>,
    /// How often to re-discover sites (0 disables re-discovery).
    discovery_interval: std::time::Duration,
    /// Last time discovery ran (startup or periodic).
    last_discovery: std::time::Instant,
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
            discovery_interval: std::time::Duration::ZERO,
            last_discovery: std::time::Instant::now(),
        }
    }

    /// Enable periodic site re-discovery (consumer.discovery_interval_secs).
    /// 0 keeps the startup-only behaviour.
    pub fn with_discovery_interval(mut self, secs: u64) -> Self {
        self.discovery_interval = std::time::Duration::from_secs(secs);
        self
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
        let sites = self.sync_groups().await?;

        info!(
            site_count = sites,
            consumer_group = CONSUMER_GROUP,
            "Initialized consumer groups"
        );

        Ok(())
    }

    /// Discover sites and ensure a consumer group per stream. Idempotent
    /// (XGROUP CREATE tolerates BUSYGROUP); discover_sites() logs site
    /// additions/removals itself, so this stays quiet otherwise.
    async fn sync_groups(&mut self) -> Result<usize> {
        let sites = self.site_discovery.discover_sites().await?;

        for site_id in &sites {
            let stream_key = self.site_discovery.get_stream_key(site_id);
            self.ensure_consumer_group(&stream_key).await?;
        }

        self.last_discovery = std::time::Instant::now();
        Ok(sites.len())
    }

    /// Re-run site discovery when the configured interval has elapsed, so
    /// sites onboarded after daemon start are picked up without a restart.
    /// A discovery failure is logged and retried next interval — it must
    /// never take down the read loop for the already-known sites.
    async fn maybe_rediscover(&mut self) {
        if self.discovery_interval.is_zero()
            || self.last_discovery.elapsed() < self.discovery_interval
        {
            return;
        }
        if let Err(e) = self.sync_groups().await {
            warn!(error = %e, "Periodic site re-discovery failed; will retry next interval");
            self.last_discovery = std::time::Instant::now();
        }
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
        // Periodic re-discovery: refreshes known_sites + consumer groups on
        // the configured interval, so the key list below tracks new sites.
        self.maybe_rediscover().await;

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

    /// Re-fetch a single archived-but-unACKed-or-ACKed entry from stream
    /// history (streams retain entries after XACK until trimmed) and parse
    /// it. Used by the retry loop to re-dispatch failed channels.
    pub async fn fetch_message(
        &mut self,
        stream_key: &str,
        entry_id: &str,
    ) -> Result<Option<CommsMessage>> {
        let reply: Vec<(String, Vec<(String, String)>)> = redis::cmd("XRANGE")
            .arg(stream_key)
            .arg(entry_id)
            .arg(entry_id)
            .query_async(&mut self.conn)
            .await
            .map_err(CommsError::ValKey)?;

        let Some((id, fields)) = reply.into_iter().next() else {
            return Ok(None);
        };
        let entry = StreamEntry {
            id,
            fields: fields.into_iter().collect(),
        };
        self.parse_message(stream_key, &entry).map(Some)
    }

    /// Parse a stream entry into a CommsMessage
    fn parse_message(&self, stream_key: &str, entry: &StreamEntry) -> Result<CommsMessage> {
        let fields = &entry.fields;

        // Extract site_id from brace-literal hash-tagged stream key
        // (e.g. "{mysite}:gnode:comms:production" → "mysite").
        let site_id = stream_key
            .split(":gnode:comms:")
            .next()
            .unwrap_or("unknown")
            .trim_start_matches('{')
            .trim_end_matches('}')
            .to_string();

        // path-traversal + injection defence before
        // site_id flows into data_dir.join() / ValKey key construction.
        validate_site_id(&site_id)?;

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

        // Parse sender JSON (size-capped per an earlier hardening pass)
        let sender = fields
            .get("sender")
            .and_then(|s| parse_field_safely("sender", s));

        // Parse content JSON (size-capped per an earlier hardening pass)
        let content = fields
            .get("content")
            .and_then(|s| parse_field_safely("content", s))
            .unwrap_or_else(|| crate::channels::MessageContent {
                subject: fields.get("subject").cloned(),
                body: fields.get("body").or(fields.get("message")).cloned(),
                attachments: HashMap::new(),
            });

        // Parse metadata JSON (size-capped per an earlier hardening pass)
        let metadata: HashMap<String, serde_json::Value> = fields
            .get("metadata")
            .and_then(|s| parse_field_safely("metadata", s))
            .unwrap_or_default();

        // Parse dispatch JSON (size-capped per an earlier hardening pass)
        let dispatch = fields
            .get("dispatch")
            .and_then(|s| parse_field_safely("dispatch", s));

        // Resolve the DTAP environment for side-effect gating (crate::dtap).
        // A producer-stamped `environment` field is authoritative — it catches
        // a non-prod message mis-XADDed onto the production stream. Absent that,
        // fall back to the stream-key suffix (always present), so legacy
        // messages on the production stream resolve to "production" and are not
        // gated (no rollout outage). Fail-safe: an unstamped, unparseable key
        // yields "unknown" → non-production → gated.
        let environment = fields
            .get("environment")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| crate::dtap::environment_from_stream_key(stream_key).map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());

        Ok(CommsMessage {
            id,
            message_type,
            timestamp,
            site_id,
            priority,
            sender,
            content,
            metadata,
            environment,
            dispatch,
        })
    }

    /// Acknowledge a message as processed
    pub async fn ack_message(&mut self, stream_key: &str, message_id: &str) -> Result<()> {
        redis::cmd("XACK")
            .arg(stream_key)
            .arg(CONSUMER_GROUP)
            .arg(message_id)
            .query_async::<()>(&mut self.conn)
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
            redis::Value::Array(arr) if arr.len() >= 2 => {
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
            redis::Value::Array(arr) if arr.len() >= 2 => {
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
