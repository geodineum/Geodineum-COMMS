//! Reply contexts: an alert's reply options and a poll's buttons, kept so an operator's reply or press reaches
//! the component that asked. (The per-chat pipeline and session state left with the Geodine paths, 2026-11-09.)

use chrono::Utc;
use redis::aio::MultiplexedConnection;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::debug;

use crate::error::{CommsError, Result};

/// Context TTL: 24 hours for reply-correlation contexts
const CONTEXT_TTL_SECS: i64 = 24 * 60 * 60;

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

/// Reply contexts in ValKey.
///
/// Key schemas:
///   {site_id}:comms:context:{context_id}   → Hash (ContextEntry fields) TTL:24h
///   {site_id}:comms:active_context:{chat_id} → most recent context_id for this chat
pub struct ConversationState {
    conn: MultiplexedConnection,
}

impl ConversationState {
    pub fn new(conn: MultiplexedConnection) -> Self {
        Self { conn }
    }

    /// Context hash key for a context_id
    fn ctx_key(site_id: &str, context_id: &str) -> String {
        format!("{{{}}}:comms:context:{}", site_id, context_id)
    }

    /// Active context pointer for a chat
    fn active_ctx_key(site_id: &str, chat_id: &str) -> String {
        format!("{{{}}}:comms:active_context:{}", site_id, chat_id)
    }

    /// Track a context for reply-correlation.
    /// Called when an outbound alert has reply_options (e.g., QUARANTINE/DISMISS).
    /// Track a context that will be resolved by CALLBACK (inline button), not
    /// by a typed reply. Buttons carry the context id in their callback_data,
    /// so no per-chat active-context pointer is needed — which also means the
    /// producer does not have to know a chat_id. Used by poll messages.
    pub async fn track_callback_context(
        &mut self,
        site_id: &str,
        context_id: &str,
        component: &str,
        reply_options: &[String],
        callback_stream: &str,
    ) -> Result<()> {
        let ctx_key = Self::ctx_key(site_id, context_id);
        let options_json = serde_json::to_string(reply_options)
            .map_err(|e| CommsError::Internal(format!("Failed to serialize reply_options: {}", e)))?;
        let now = Utc::now().to_rfc3339();
        let fields: Vec<(&str, &str)> = vec![
            ("component", component),
            ("reply_options", &options_json),
            ("callback_stream", callback_stream),
            ("created_at", &now),
        ];
        redis::pipe()
            .cmd("HSET").arg(&ctx_key).arg(&fields)
            .cmd("EXPIRE").arg(&ctx_key).arg(CONTEXT_TTL_SECS)
            .query_async::<()>(&mut self.conn)
            .await
            .map_err(|e| CommsError::Internal(format!("Failed to track callback context: {}", e)))?;
        Ok(())
    }

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
}
