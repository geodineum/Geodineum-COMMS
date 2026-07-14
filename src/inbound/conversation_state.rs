//! Conversation state management — ValKey-backed sessions and context tracking
//!
//! Ported from the legacy Telegram bot's SessionManager + ConversationStateManager patterns,
//! adapted for ValKey hashes with TTL instead of SQLite.

use chrono::{DateTime, Duration, Utc};
use redis::aio::MultiplexedConnection;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info};

use crate::error::{CommsError, Result};

/// Session timeout: auto-conclude after 30min inactivity (matches the legacy bot)
const SESSION_TIMEOUT_SECS: i64 = 30 * 60;

/// Context TTL: 24 hours for reply-correlation contexts
const CONTEXT_TTL_SECS: i64 = 24 * 60 * 60;

/// Conversation state for an operator chat session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvState {
    /// Which pipeline is active for this chat
    pub pipeline: String,
    /// Server-side session ID (for inference-service ConversationStore)
    pub session_id: String,
    /// Last command text
    pub last_command: String,
    /// ISO-8601 timestamp of last activity
    pub last_activity: String,
    /// Operator identifier
    pub operator_id: String,
    /// Operator display name
    pub operator_name: String,
    /// Message count in this session
    pub message_count: u32,
}

/// Context for reply-correlation (tracks interactive alerts)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    /// Source component that sent the alert
    pub component: String,
    /// Valid reply options (e.g., ["QUARANTINE", "DISMISS"])
    pub reply_options: Vec<String>,
    /// Stream to write the reply back to
    pub callback_stream: String,
    /// Original message context for reference
    pub original_message_id: String,
    /// When this context was created
    pub created_at: String,
}

/// Result of resolving an operator reply against active contexts
#[derive(Debug, Clone)]
pub struct ContextResolution {
    /// The context that matched
    pub context_id: String,
    /// Target component to route the command to
    pub component: String,
    /// The validated command
    pub command: String,
    /// Stream to write the routed command to
    pub callback_stream: String,
}

/// Manages conversation state in ValKey hashes.
///
/// Key schemas:
///   {site_id}:comms:conversation:{chat_id} → Hash (ConvState fields)
///   {site_id}:comms:context:{context_id}   → Hash (ContextEntry fields) TTL:24h
///   {site_id}:comms:active_context:{chat_id} → most recent context_id for this chat
pub struct ConversationState {
    conn: MultiplexedConnection,
    default_pipeline: String,
}

impl ConversationState {
    pub fn new(conn: MultiplexedConnection, default_pipeline: String) -> Self {
        Self {
            conn,
            default_pipeline,
        }
    }

    /// Conversation hash key for a chat
    fn conv_key(site_id: &str, chat_id: &str) -> String {
        format!("{{{}}}:comms:conversation:{}", site_id, chat_id)
    }

    /// Context hash key for a context_id
    fn ctx_key(site_id: &str, context_id: &str) -> String {
        format!("{{{}}}:comms:context:{}", site_id, context_id)
    }

    /// Active context pointer for a chat
    fn active_ctx_key(site_id: &str, chat_id: &str) -> String {
        format!("{{{}}}:comms:active_context:{}", site_id, chat_id)
    }

    /// Get or create conversation state for a chat.
    /// If the session has timed out (30min inactivity), auto-concludes and creates a new one.
    pub async fn get_or_create(
        &mut self,
        chat_id: &str,
        site_id: &str,
        operator_id: &str,
        operator_name: &str,
    ) -> Result<ConvState> {
        let key = Self::conv_key(site_id, chat_id);

        // an earlier hardening pass: HGETALL + HSET-touch in a single MULTI/EXEC transaction
        // so two concurrent message-handlers for the same chat can't both
        // observe the pre-touch state and race on the touch. The
        // create-fresh path below is wrapped with HSETNX-style semantics
        // (HSET only when key was empty) to keep parallel session creation
        // last-writer-wins-but-never-lost-fields rather than splitting
        // a session across two operator turns.
        let mut pipe = redis::pipe();
        let now = Utc::now().to_rfc3339();
        pipe.atomic()
            .cmd("HGETALL").arg(&key)
            .cmd("HSET").arg(&key).arg("last_activity").arg(&now);
        let (fields, _hset_count): (HashMap<String, String>, i64) = pipe
            .query_async(&mut self.conn)
            .await
            .map_err(CommsError::ValKey)?;

        if !fields.is_empty() {
            let state = Self::parse_conv_state(&fields, &self.default_pipeline);

            // Check for session timeout (30min inactivity, matching the legacy bot).
            // The HSET above already refreshed last_activity, but the
            // PRE-touch state we just read still reflects whether the
            // user was idle past the threshold — that's the signal we
            // gate session-renewal on.
            if let Ok(last) = DateTime::parse_from_rfc3339(&state.last_activity) {
                let idle = Utc::now() - last.with_timezone(&Utc);
                if idle > Duration::seconds(SESSION_TIMEOUT_SECS) {
                    info!(
                        chat_id = %chat_id,
                        idle_mins = idle.num_minutes(),
                        "Session timed out, creating new session"
                    );
                    return self
                        .create_session(chat_id, site_id, operator_id, operator_name)
                        .await;
                }
            }

            return Ok(state);
        }

        // Empty fields == no existing session. The HSET above wrote a
        // single `last_activity` field on a fresh key, which create_session
        // will overwrite with the full session state. No data loss.
        self.create_session(chat_id, site_id, operator_id, operator_name)
            .await
    }

    /// Create a new conversation session
    async fn create_session(
        &mut self,
        chat_id: &str,
        site_id: &str,
        operator_id: &str,
        operator_name: &str,
    ) -> Result<ConvState> {
        let key = Self::conv_key(site_id, chat_id);
        let now = Utc::now().to_rfc3339();
        let session_id = format!("tg_{}_{}", chat_id, Utc::now().timestamp());

        let state = ConvState {
            pipeline: self.default_pipeline.clone(),
            session_id: session_id.clone(),
            last_command: String::new(),
            last_activity: now.clone(),
            operator_id: operator_id.to_string(),
            operator_name: operator_name.to_string(),
            message_count: 0,
        };

        let fields: Vec<(&str, String)> = vec![
            ("pipeline", state.pipeline.clone()),
            ("session_id", state.session_id.clone()),
            ("last_command", state.last_command.clone()),
            ("last_activity", state.last_activity.clone()),
            ("operator_id", state.operator_id.clone()),
            ("operator_name", state.operator_name.clone()),
            ("message_count", state.message_count.to_string()),
        ];

        let field_refs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();

        redis::cmd("HSET")
            .arg(&key)
            .arg(&field_refs)
            .query_async::<()>(&mut self.conn)
            .await
            .map_err(CommsError::ValKey)?;

        info!(
            chat_id = %chat_id,
            session_id = %session_id,
            "Created new conversation session"
        );

        Ok(state)
    }

    /// Set the active pipeline for a chat
    pub async fn set_pipeline(
        &mut self,
        chat_id: &str,
        site_id: &str,
        pipeline: &str,
    ) -> Result<()> {
        let key = Self::conv_key(site_id, chat_id);

        redis::cmd("HSET")
            .arg(&key)
            .arg("pipeline")
            .arg(pipeline)
            .query_async::<()>(&mut self.conn)
            .await
            .map_err(CommsError::ValKey)?;

        debug!(chat_id = %chat_id, pipeline = %pipeline, "Pipeline updated");
        Ok(())
    }

    /// Increment message count and update last command
    pub async fn record_message(
        &mut self,
        chat_id: &str,
        site_id: &str,
        command_text: &str,
    ) -> Result<()> {
        let key = Self::conv_key(site_id, chat_id);
        let now = Utc::now().to_rfc3339();

        // HINCRBY + HSET in pipeline
        redis::pipe()
            .cmd("HINCRBY")
            .arg(&key)
            .arg("message_count")
            .arg(1)
            .cmd("HSET")
            .arg(&key)
            .arg("last_command")
            .arg(command_text)
            .arg("last_activity")
            .arg(&now)
            .query_async::<()>(&mut self.conn)
            .await
            .map_err(CommsError::ValKey)?;

        Ok(())
    }

    /// Track a context for reply-correlation.
    /// Called when an outbound alert has reply_options (e.g., QUARANTINE/DISMISS).
    pub async fn track_context(
        &mut self,
        site_id: &str,
        chat_id: &str,
        context_id: &str,
        component: &str,
        reply_options: &[String],
        callback_stream: &str,
        original_message_id: &str,
    ) -> Result<()> {
        let ctx_key = Self::ctx_key(site_id, context_id);
        let active_key = Self::active_ctx_key(site_id, chat_id);
        let now = Utc::now().to_rfc3339();

        let options_json = serde_json::to_string(reply_options)
            .map_err(|e| CommsError::Internal(format!("Failed to serialize reply_options: {}", e)))?;

        let fields: Vec<(&str, &str)> = vec![
            ("component", component),
            ("reply_options", &options_json),
            ("callback_stream", callback_stream),
            ("original_message_id", original_message_id),
            ("created_at", &now),
        ];

        // Set context hash with TTL + update active context pointer
        redis::pipe()
            .cmd("HSET")
            .arg(&ctx_key)
            .arg(&fields)
            .cmd("EXPIRE")
            .arg(&ctx_key)
            .arg(CONTEXT_TTL_SECS)
            .cmd("SET")
            .arg(&active_key)
            .arg(context_id)
            .arg("EX")
            .arg(CONTEXT_TTL_SECS)
            .query_async::<()>(&mut self.conn)
            .await
            .map_err(CommsError::ValKey)?;

        debug!(
            context_id = %context_id,
            component = %component,
            reply_options = ?reply_options,
            "Tracked context for reply-correlation"
        );

        Ok(())
    }

    /// Resolve an operator reply against active contexts.
    /// When operator replies "QUARANTINE", find the active context, validate
    /// against reply_options, return the target component and callback stream.
    pub async fn resolve_reply(
        &mut self,
        chat_id: &str,
        site_id: &str,
        reply_text: &str,
    ) -> Result<Option<ContextResolution>> {
        let active_key = Self::active_ctx_key(site_id, chat_id);

        // Get active context_id for this chat
        let context_id: Option<String> = self
            .conn
            .get(&active_key)
            .await
            .map_err(CommsError::ValKey)?;

        let context_id = match context_id {
            Some(id) => id,
            None => return Ok(None), // No active context
        };

        let ctx_key = Self::ctx_key(site_id, &context_id);

        // Load context
        let fields: HashMap<String, String> = redis::cmd("HGETALL")
            .arg(&ctx_key)
            .query_async(&mut self.conn)
            .await
            .map_err(CommsError::ValKey)?;

        if fields.is_empty() {
            return Ok(None); // Context expired
        }

        // Parse reply_options
        let reply_options: Vec<String> = fields
            .get("reply_options")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        // Normalize the reply text for matching
        let normalized = reply_text.trim().to_uppercase();

        // Check if the reply matches any valid option
        if !reply_options.iter().any(|opt| opt.to_uppercase() == normalized) {
            debug!(
                reply = %reply_text,
                valid_options = ?reply_options,
                "Reply does not match any valid option"
            );
            return Ok(None);
        }

        let component = fields
            .get("component")
            .cloned()
            .unwrap_or_default();
        let callback_stream = fields
            .get("callback_stream")
            .cloned()
            .unwrap_or_default();

        // Clear the active context (idempotent — same reply only processed once)
        let _: () = self
            .conn
            .del(&active_key)
            .await
            .map_err(CommsError::ValKey)?;

        Ok(Some(ContextResolution {
            context_id,
            component,
            command: normalized,
            callback_stream,
        }))
    }

    /// Reset conversation state for a chat (used by /reset command)
    pub async fn reset(&mut self, chat_id: &str, site_id: &str) -> Result<()> {
        let key = Self::conv_key(site_id, chat_id);
        let active_key = Self::active_ctx_key(site_id, chat_id);

        redis::pipe()
            .cmd("DEL")
            .arg(&key)
            .cmd("DEL")
            .arg(&active_key)
            .query_async::<()>(&mut self.conn)
            .await
            .map_err(CommsError::ValKey)?;

        info!(chat_id = %chat_id, "Conversation state reset");
        Ok(())
    }

    /// Get conversation state without creating (for read-only queries)
    pub async fn get(&mut self, chat_id: &str, site_id: &str) -> Result<Option<ConvState>> {
        let key = Self::conv_key(site_id, chat_id);

        let fields: HashMap<String, String> = redis::cmd("HGETALL")
            .arg(&key)
            .query_async(&mut self.conn)
            .await
            .map_err(CommsError::ValKey)?;

        if fields.is_empty() {
            return Ok(None);
        }

        Ok(Some(Self::parse_conv_state(&fields, &self.default_pipeline)))
    }

    /// Parse a ConvState from a hash field map
    fn parse_conv_state(fields: &HashMap<String, String>, default_pipeline: &str) -> ConvState {
        ConvState {
            pipeline: fields
                .get("pipeline")
                .cloned()
                .unwrap_or_else(|| default_pipeline.to_string()),
            session_id: fields
                .get("session_id")
                .cloned()
                .unwrap_or_default(),
            last_command: fields
                .get("last_command")
                .cloned()
                .unwrap_or_default(),
            last_activity: fields
                .get("last_activity")
                .cloned()
                .unwrap_or_default(),
            operator_id: fields
                .get("operator_id")
                .cloned()
                .unwrap_or_default(),
            operator_name: fields
                .get("operator_name")
                .cloned()
                .unwrap_or_default(),
            message_count: fields
                .get("message_count")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        }
    }
}
