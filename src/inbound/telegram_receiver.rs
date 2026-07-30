//! Telegram inbound receiver — long-polls getUpdates API and writes to inbound stream
//!
//! Supports: messages, callback queries (inline buttons), group chat mention
//! detection, and BotFather command registration.

use redis::aio::MultiplexedConnection;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::error::{CommsError, Result};

/// Inbound message parsed from a Telegram update (channel-agnostic)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    /// Operator text (the message content)
    pub text: String,
    /// Telegram chat_id (where to reply)
    pub chat_id: String,
    /// Telegram user_id (operator identity)
    pub operator_id: String,
    /// Operator display name
    pub operator_name: String,
    /// Channel source identifier
    pub channel_source: String,
    /// Telegram message_id of the reply target (if replying to a bot message)
    pub reply_to_msg_id: Option<i64>,
    /// ISO-8601 timestamp
    pub timestamp: String,
    /// Chat type: "private", "group", "supergroup", "channel"
    #[serde(default)]
    pub chat_type: String,
    /// Whether this is a callback query (button press) rather than a text message
    #[serde(default)]
    pub is_callback: bool,
    /// Callback query ID (must be answered with answerCallbackQuery)
    #[serde(default)]
    pub callback_query_id: String,
    /// Message ID that contained the inline keyboard (for editing)
    #[serde(default)]
    pub callback_message_id: Option<i64>,
}

/// Telegram Bot API update object
#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMsg>,
    callback_query: Option<CallbackQuery>,
}

/// Callback query from an inline keyboard button press
#[derive(Debug, Deserialize)]
struct CallbackQuery {
    id: String,
    from: TelegramUser,
    message: Option<TelegramMsg>,
    data: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramMsg {
    message_id: i64,
    from: Option<TelegramUser>,
    chat: TelegramChat,
    text: Option<String>,
    reply_to_message: Option<Box<TelegramMsg>>,
    caption: Option<String>,
    #[serde(default)]
    date: i64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // serde shape — username surfaces via Debug logging
struct TelegramUser {
    id: i64,
    first_name: String,
    last_name: Option<String>,
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
    #[serde(rename = "type")]
    chat_type: String,
}

/// Inline keyboard button
#[derive(Debug, Clone, Serialize)]
pub struct InlineButton {
    pub text: String,
    pub callback_data: String,
}

/// Build an inline keyboard (rows of buttons)
pub fn build_inline_keyboard(rows: Vec<Vec<InlineButton>>) -> serde_json::Value {
    let keyboard: Vec<Vec<serde_json::Value>> = rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|btn| {
                    serde_json::json!({
                        "text": btn.text,
                        "callback_data": btn.callback_data,
                    })
                })
                .collect()
        })
        .collect();
    serde_json::json!({ "inline_keyboard": keyboard })
}

#[derive(Debug, Deserialize)]
struct GetUpdatesResponse {
    ok: bool,
    result: Option<Vec<TelegramUpdate>>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SendMessageResponse {
    ok: bool,
    result: Option<SendMessageResult>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SendMessageResult {
    message_id: i64,
}

/// Receives inbound Telegram messages via long-polling and writes them
/// to the ValKey inbound stream for processing by the command router.
pub struct TelegramReceiver {
    client: Client,
    /// Reserved for direct-API request paths; current flow embeds the token
    /// into `api_base` at construction time (long-poll URL).
    #[allow(dead_code)]
    bot_token: String,
    api_base: String,
    /// Last processed update_id (offset for getUpdates)
    offset: i64,
    /// Long-poll timeout in seconds
    poll_timeout: u64,
    /// ValKey connection for writing inbound stream
    conn: MultiplexedConnection,
    /// Site ID this receiver is bound to
    site_id: String,
    /// Environment (production, staging, etc.)
    environment: String,
    /// Authorized operator user IDs. Commit 1.6.d fail-closed
    /// semantics: `None` means COMMS_ADMIN_IDS was never set → allow-all
    /// (dev/test mode). `Some(list)` means the gate is explicitly
    /// configured — even an empty list rejects everyone, which is the
    /// intended behavior when an operator sets the env var to a
    /// mis-parsed value (e.g. `junk,stuff`).
    admin_ids: Option<Vec<i64>>,
    /// Bot's own username (populated on first getMe call)
    bot_username: Option<String>,
    /// Bot's own user_id (populated on first getMe call). an earlier hardening pass:
    /// reply-to-bot detection in group chats now compares
    /// `reply_to_message.from.id == bot_user_id` instead of the prior
    /// "any reply has bot context" heuristic that any group member
    /// could trip by replying to an old bot message.
    bot_user_id: Option<i64>,
}

impl TelegramReceiver {
    pub fn new(
        bot_token: String,
        conn: MultiplexedConnection,
        site_id: String,
        environment: String,
        admin_ids: Option<Vec<i64>>,
    ) -> Self {
        Self {
            client: Client::new(),
            api_base: format!("https://api.telegram.org/bot{}", bot_token),
            bot_token,
            offset: 0,
            poll_timeout: 30,
            conn,
            site_id,
            environment,
            admin_ids,
            bot_username: None,
            bot_user_id: None,
        }
    }

    /// Get the inbound stream key for this site
    fn inbound_stream_key(&self) -> String {
        format!(
            "{{{}}}:gnode:comms:inbound:{}",
            self.site_id, self.environment
        )
    }

    /// Run the polling loop until the cancellation token is triggered.
    /// On startup, fetches bot username and registers commands with BotFather.
    pub async fn start_polling(&mut self, cancel: CancellationToken) {
        // Fetch bot identity for group chat mention detection (username
        // for @-mention check, user_id for reply-to-bot check per an earlier hardening pass)
        if let Ok((username, user_id)) = self.fetch_bot_identity().await {
            info!(username = %username, user_id, "Bot identity resolved");
            self.bot_username = Some(username);
            self.bot_user_id = Some(user_id);
        }

        // Register commands with BotFather
        if let Err(e) = self.register_commands().await {
            error!(error = %e, "Failed to register commands with BotFather");
        }

        info!(
            site_id = %self.site_id,
            "Starting Telegram inbound polling"
        );

        // an earlier hardening pass: exponential backoff with jitter on consecutive errors.
        // Fixed 5s burn was 12 req/min on a 429 — enough to keep the bot
        // tagged for the API-wide rate window. Reset to 5s after every
        // successful poll. Cap at 300s. Jitter is uniform ±20% to avoid
        // thundering-herd if multiple receivers reconnect together.
        let mut consecutive_errors: u32 = 0;
        const BASE_BACKOFF_SECS: u64 = 5;
        const MAX_BACKOFF_SECS: u64 = 300;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("Telegram polling cancelled");
                    break;
                }
                result = self.poll_once() => {
                    match result {
                        Ok(count) => {
                            consecutive_errors = 0;
                            if count > 0 {
                                debug!(count, "Processed inbound updates");
                            }
                        }
                        Err(e) => {
                            consecutive_errors = consecutive_errors.saturating_add(1);
                            // Exponential: 5, 10, 20, 40, 80, 160, 300 (capped).
                            let exp = BASE_BACKOFF_SECS
                                .saturating_mul(1u64 << (consecutive_errors - 1).min(6))
                                .min(MAX_BACKOFF_SECS);
                            // Jitter: ±20%. Pseudo-random source: low bits
                            // of the system clock — adequate for retry
                            // de-correlation, no need for a real RNG.
                            let jitter_pct = (std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.subsec_nanos() as u64)
                                .unwrap_or(0)
                                % 41) as i64
                                - 20; // -20..+20
                            let jittered = (exp as i64).saturating_add(exp as i64 * jitter_pct / 100);
                            let sleep_secs = jittered.max(1) as u64;
                            error!(error = %e, attempt = consecutive_errors, sleep_secs, "Telegram polling error — backing off");
                            tokio::time::sleep(tokio::time::Duration::from_secs(sleep_secs)).await;
                        }
                    }
                }
            }
        }
    }

    /// Fetch bot identity (username + user_id) via getMe API.
    /// The user_id is required for the an earlier hardening pass reply-to-bot check.
    async fn fetch_bot_identity(&self) -> Result<(String, i64)> {
        let url = format!("{}/getMe", self.api_base);
        let resp: serde_json::Value = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| CommsError::Telegram(format!("getMe failed: {}", e)))?
            .json()
            .await
            .map_err(|e| CommsError::Telegram(format!("getMe parse failed: {}", e)))?;

        let result = resp
            .get("result")
            .ok_or_else(|| CommsError::Telegram("getMe: missing 'result' field".into()))?;

        let username = result
            .get("username")
            .and_then(|u| u.as_str())
            .map(|s| format!("@{}", s))
            .ok_or_else(|| CommsError::Telegram("getMe: no username".into()))?;

        let user_id = result
            .get("id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| CommsError::Telegram("getMe: no id".into()))?;

        Ok((username, user_id))
    }

    /// Register bot commands with BotFather (shows in Telegram UI)
    pub async fn register_commands(&self) -> Result<()> {
        let url = format!("{}/setMyCommands", self.api_base);
        let commands = serde_json::json!({
            "commands": [
                {"command": "start", "description": "Start / show menu"},
                {"command": "reset", "description": "Reset conversation"},
                {"command": "history", "description": "View conversation history"},
                {"command": "pipeline", "description": "Switch active pipeline"},
                {"command": "status", "description": "System status"},
                {"command": "help", "description": "Show available commands"},
            ]
        });

        self.client
            .post(&url)
            .json(&commands)
            .send()
            .await
            .map_err(|e| CommsError::Telegram(format!("setMyCommands failed: {}", e)))?;

        info!("Registered bot commands with BotFather");
        Ok(())
    }

    /// Check if a message in a group chat is directed at this bot
    fn is_mentioned_in_group(&self, msg: &TelegramMsg) -> bool {
        if msg.chat.chat_type == "private" {
            return true; // Always process private chats
        }

        // Group/supergroup: only respond if @mentioned or reply to bot
        let mention = self.bot_username.as_deref().unwrap_or("@GeodineBot");

        // Check text for @mention
        let text = msg.text.as_deref().or(msg.caption.as_deref()).unwrap_or("");
        if text.contains(mention) {
            return true;
        }

        // an earlier hardening pass: only treat the message as bot-directed if the reply
        // target is a message authored by THIS bot. Without the
        // `from.id == bot_user_id` check, any group member's reply to an
        // old bot message would route through inference/dispatch — and
        // if their telegram ID slipped into admin_ids, that's a workflow-engine
        // RCE primitive (couples with an earlier hardening pass). The reply check now
        // matches the explicit an earlier hardening pass prescription.
        if let (Some(reply), Some(my_id)) = (msg.reply_to_message.as_ref(), self.bot_user_id) {
            if let Some(from) = reply.from.as_ref() {
                if from.id == my_id {
                    return true;
                }
            }
        }

        // Check for slash commands (always process these in groups)
        if text.starts_with('/') {
            return true;
        }

        false
    }

    /// Single poll iteration: call getUpdates, parse messages + callback queries
    async fn poll_once(&mut self) -> Result<usize> {
        let url = format!(
            "{}/getUpdates?offset={}&timeout={}&allowed_updates=[\"message\",\"callback_query\"]",
            self.api_base, self.offset, self.poll_timeout
        );

        let response = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(self.poll_timeout + 10))
            .send()
            .await
            .map_err(|e| CommsError::Telegram(format!("getUpdates failed: {}", e)))?;

        let body: GetUpdatesResponse = response
            .json()
            .await
            .map_err(|e| CommsError::Telegram(format!("Failed to parse getUpdates: {}", e)))?;

        if !body.ok {
            return Err(CommsError::Telegram(
                body.description.unwrap_or_else(|| "getUpdates not ok".into()),
            ));
        }

        let updates = body.result.unwrap_or_default();
        let count = updates.len();

        for update in updates {
            self.offset = update.update_id + 1;

            // Handle callback queries (inline keyboard button presses)
            if let Some(cb) = update.callback_query {
                let user_id = cb.from.id;
                // an earlier hardening pass fail-closed gate: Some([...]) = explicitly
                // configured whitelist; None = allow-all (unset).
                if let Some(allowed) = &self.admin_ids {
                    if !allowed.contains(&user_id) {
                        continue;
                    }
                }

                let chat_id = cb
                    .message
                    .as_ref()
                    .map(|m| m.chat.id.to_string())
                    .unwrap_or_default();
                let callback_msg_id = cb.message.as_ref().map(|m| m.message_id);

                let operator_name = match &cb.from.last_name {
                    Some(last) => format!("{} {}", cb.from.first_name, last),
                    None => cb.from.first_name.clone(),
                };

                let inbound = InboundMessage {
                    text: cb.data.unwrap_or_default(),
                    chat_id,
                    operator_id: user_id.to_string(),
                    operator_name,
                    channel_source: "telegram".to_string(),
                    reply_to_msg_id: None,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    chat_type: "private".to_string(), // callbacks treated as private
                    is_callback: true,
                    callback_query_id: cb.id,
                    callback_message_id: callback_msg_id,
                };

                let cb_id = inbound.callback_query_id.clone();
                match self.write_to_stream(&inbound).await {
                    Ok(()) => {
                        // Telegram spins the button until answerCallbackQuery
                        // arrives, so an unanswered press reads as a failure and
                        // gets pressed again. Answered AFTER the write, so the
                        // acknowledgement means recorded, not merely received.
                        let _ = self.answer_callback_query(&cb_id, Some("Recorded")).await;
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to write callback to stream");
                        let _ = self
                            .answer_callback_query(&cb_id, Some("Not recorded — press again"))
                            .await;
                    }
                }
                continue;
            }

            // Handle regular messages
            if let Some(msg) = update.message {
                // Group chat filter: only process if mentioned or replied-to
                if !self.is_mentioned_in_group(&msg) {
                    continue;
                }

                if let Some(inbound) = self.parse_update(&msg) {
                    let user_id: i64 = inbound.operator_id.parse().unwrap_or(0);
                    // an earlier hardening pass fail-closed gate (see above).
                    let unauthorized = matches!(&self.admin_ids, Some(allowed) if !allowed.contains(&user_id));
                    if unauthorized {
                        debug!(
                            operator_id = %inbound.operator_id,
                            "Ignoring message from unauthorized user"
                        );
                        continue;
                    }

                    if let Err(e) = self.write_to_stream(&inbound).await {
                        error!(
                            error = %e,
                            chat_id = %inbound.chat_id,
                            "Failed to write inbound message to stream"
                        );
                    }
                }
            }
        }

        Ok(count)
    }

    /// Parse a Telegram message into a channel-agnostic InboundMessage
    fn parse_update(&self, msg: &TelegramMsg) -> Option<InboundMessage> {
        // Accept text or caption (for photos/documents with captions)
        let text = msg.text.as_ref().or(msg.caption.as_ref())?;
        let from = msg.from.as_ref()?;

        let operator_name = match &from.last_name {
            Some(last) => format!("{} {}", from.first_name, last),
            None => from.first_name.clone(),
        };

        let reply_to_msg_id = msg
            .reply_to_message
            .as_ref()
            .map(|r| r.message_id);

        let timestamp = chrono::DateTime::from_timestamp(msg.date, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

        // Strip bot mention from group messages for cleaner routing
        let mut clean_text = text.clone();
        if let Some(ref mention) = self.bot_username {
            clean_text = clean_text.replace(mention, "").trim().to_string();
        }

        Some(InboundMessage {
            text: clean_text,
            chat_id: msg.chat.id.to_string(),
            operator_id: from.id.to_string(),
            operator_name,
            channel_source: "telegram".to_string(),
            reply_to_msg_id,
            timestamp,
            chat_type: msg.chat.chat_type.clone(),
            is_callback: false,
            callback_query_id: String::new(),
            callback_message_id: None,
        })
    }

    /// Write an inbound message to the ValKey stream
    async fn write_to_stream(&mut self, msg: &InboundMessage) -> Result<()> {
        let stream_key = self.inbound_stream_key();

        let fields: Vec<(&str, String)> = vec![
            ("type", "telegram".to_string()),
            ("chat_id", msg.chat_id.clone()),
            ("operator_id", msg.operator_id.clone()),
            ("operator_name", msg.operator_name.clone()),
            ("text", msg.text.clone()),
            (
                "reply_to_msg_id",
                msg.reply_to_msg_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            ),
            ("channel_source", msg.channel_source.clone()),
            ("ts", msg.timestamp.clone()),
            ("chat_type", msg.chat_type.clone()),
            ("is_callback", msg.is_callback.to_string()),
            ("callback_query_id", msg.callback_query_id.clone()),
            (
                "callback_message_id",
                msg.callback_message_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            ),
        ];

        let field_refs: Vec<(&str, &str)> = fields
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect();

        redis::cmd("XADD")
            .arg(&stream_key)
            .arg("*")
            .arg(&field_refs)
            .query_async::<String>(&mut self.conn)
            .await
            .map_err(CommsError::ValKey)?;

        debug!(
            stream = %stream_key,
            chat_id = %msg.chat_id,
            // an earlier hardening pass: operator_name is attacker-controlled Telegram
            // first_name+last_name; normalize before tracing.
            operator = %crate::inbound::log_safe(&msg.operator_name),
            "Wrote inbound message to stream"
        );

        Ok(())
    }

    /// Send a text message to a Telegram chat. Returns the message_id.
    pub async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: &str,
    ) -> Result<i64> {
        let url = format!("{}/sendMessage", self.api_base);

        let payload = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": parse_mode,
        });

        let response = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| CommsError::Telegram(format!("sendMessage failed: {}", e)))?;

        let body: SendMessageResponse = response
            .json()
            .await
            .map_err(|e| CommsError::Telegram(format!("Failed to parse sendMessage: {}", e)))?;

        if body.ok {
            Ok(body.result.map(|r| r.message_id).unwrap_or(0))
        } else {
            Err(CommsError::Telegram(
                body.description.unwrap_or_else(|| "sendMessage failed".into()),
            ))
        }
    }

    /// Send typing indicator to a chat
    pub async fn send_typing(&self, chat_id: &str) -> Result<()> {
        let url = format!("{}/sendChatAction", self.api_base);

        let payload = serde_json::json!({
            "chat_id": chat_id,
            "action": "typing",
        });

        self.client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| CommsError::Telegram(format!("sendChatAction failed: {}", e)))?;

        Ok(())
    }

    /// Edit an existing message
    pub async fn edit_message(
        &self,
        chat_id: &str,
        message_id: i64,
        text: &str,
        parse_mode: &str,
    ) -> Result<()> {
        let url = format!("{}/editMessageText", self.api_base);

        let payload = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
            "parse_mode": parse_mode,
        });

        let response = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| CommsError::Telegram(format!("editMessageText failed: {}", e)))?;

        let body: SendMessageResponse = response
            .json()
            .await
            .map_err(|e| {
                CommsError::Telegram(format!("Failed to parse editMessageText: {}", e))
            })?;

        if !body.ok {
            return Err(CommsError::Telegram(
                body.description
                    .unwrap_or_else(|| "editMessageText failed".into()),
            ));
        }

        Ok(())
    }

    /// Send typing indicator every 5s until the token is cancelled.
    /// Telegram auto-expires typing after ~5s, so we must keep re-sending.
    pub async fn send_typing_loop(&self, chat_id: &str, cancel: CancellationToken) {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
                    if let Err(e) = self.send_typing(chat_id).await {
                        debug!(error = %e, "Typing indicator failed (non-fatal)");
                    }
                }
            }
        }
    }

    /// Send a message with an inline keyboard
    pub async fn send_message_with_keyboard(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: &str,
        reply_markup: &serde_json::Value,
    ) -> Result<i64> {
        let url = format!("{}/sendMessage", self.api_base);

        let payload = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": parse_mode,
            "reply_markup": reply_markup,
        });

        let response = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| CommsError::Telegram(format!("sendMessage failed: {}", e)))?;

        let body: SendMessageResponse = response
            .json()
            .await
            .map_err(|e| CommsError::Telegram(format!("parse failed: {}", e)))?;

        if body.ok {
            Ok(body.result.map(|r| r.message_id).unwrap_or(0))
        } else {
            Err(CommsError::Telegram(
                body.description.unwrap_or_else(|| "sendMessage failed".into()),
            ))
        }
    }

    /// Edit a message and optionally update its inline keyboard
    pub async fn edit_message_with_keyboard(
        &self,
        chat_id: &str,
        message_id: i64,
        text: &str,
        parse_mode: &str,
        reply_markup: Option<&serde_json::Value>,
    ) -> Result<()> {
        let url = format!("{}/editMessageText", self.api_base);

        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
            "parse_mode": parse_mode,
        });

        if let Some(markup) = reply_markup {
            payload["reply_markup"] = markup.clone();
        }

        let response = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| CommsError::Telegram(format!("editMessageText failed: {}", e)))?;

        let body: SendMessageResponse = response
            .json()
            .await
            .map_err(|e| CommsError::Telegram(format!("parse failed: {}", e)))?;

        if !body.ok {
            return Err(CommsError::Telegram(
                body.description.unwrap_or_else(|| "editMessageText failed".into()),
            ));
        }

        Ok(())
    }

    /// Answer a callback query (required by Telegram — removes "loading" spinner on button)
    pub async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> Result<()> {
        let url = format!("{}/answerCallbackQuery", self.api_base);

        let mut payload = serde_json::json!({
            "callback_query_id": callback_query_id,
        });

        if let Some(t) = text {
            payload["text"] = serde_json::Value::String(t.to_string());
        }

        self.client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| CommsError::Telegram(format!("answerCallbackQuery failed: {}", e)))?;

        Ok(())
    }
}
