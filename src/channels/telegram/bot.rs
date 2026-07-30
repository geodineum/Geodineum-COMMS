//! Telegram Bot API Provider using direct HTTP requests

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info, instrument};

use crate::channels::channel::{
    ChannelConfig, CommsMessage, NotificationChannel, RateLimit, RecipientConfig, RenderedContent,
    SendResult,
};
use crate::error::{CommsError, Result};
use crate::templates::TemplateRenderer;

/// Telegram-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: String,
    #[serde(default = "default_parse_mode")]
    pub parse_mode: String,
    #[serde(default)]
    pub disable_notification: bool,
}

fn default_parse_mode() -> String {
    "MarkdownV2".to_string()
}

impl TelegramConfig {
    /// Extract TelegramConfig from generic ChannelConfig
    pub fn from_channel_config(config: &ChannelConfig) -> Result<Self> {
        let config_json = serde_json::to_value(&config.config)
            .map_err(|e| CommsError::Config(format!("Failed to serialize config: {}", e)))?;

        serde_json::from_value(config_json)
            .map_err(|e| CommsError::Config(format!("Invalid Telegram config: {}", e)))
    }
}

/// Telegram API response
#[derive(Debug, Deserialize)]
struct TelegramResponse {
    ok: bool,
    result: Option<TelegramMessage>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    message_id: i64,
}

/// Telegram notification channel (outbound + inbound support)
pub struct TelegramChannel {
    client: Client,
    template_renderer: Arc<TemplateRenderer>,
}

impl TelegramChannel {
    pub fn new(template_renderer: Arc<TemplateRenderer>) -> Self {
        Self {
            client: Client::new(),
            template_renderer,
        }
    }

    /// Whether this channel supports inbound messages
    pub fn supports_inbound(&self) -> bool {
        true
    }

    /// Send a typing indicator to a chat
    pub async fn send_typing_action(
        &self,
        bot_token: &str,
        chat_id: &str,
    ) -> Result<()> {
        let url = format!("https://api.telegram.org/bot{}/sendChatAction", bot_token);

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

    /// Edit an existing message text
    pub async fn edit_message_text(
        &self,
        bot_token: &str,
        chat_id: &str,
        message_id: i64,
        text: &str,
        parse_mode: &str,
    ) -> Result<()> {
        let url = format!("https://api.telegram.org/bot{}/editMessageText", bot_token);

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

        let body: TelegramResponse = response
            .json()
            .await
            .map_err(|e| CommsError::Telegram(format!("Failed to parse edit response: {}", e)))?;

        if !body.ok {
            return Err(CommsError::Telegram(
                body.description.unwrap_or_else(|| "editMessageText failed".into()),
            ));
        }

        Ok(())
    }

    /// Escape special characters for MarkdownV2
    fn escape_markdown_v2(text: &str) -> String {
        let special_chars = [
            '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
        ];
        let mut result = String::with_capacity(text.len() * 2);
        for c in text.chars() {
            if special_chars.contains(&c) {
                result.push('\\');
            }
            result.push(c);
        }
        result
    }

    /// Send a message via the Telegram Bot API, with an optional inline
    /// keyboard.
    ///
    /// Buttons make a notification ACTIONABLE, which is the difference between
    /// an approval loop you can answer from a phone and one that auto-denies
    /// after 72 hours because nobody was at a terminal.
    ///
    /// Deliberately NOT a link with a token in it. A capability URL in a
    /// message is a bearer credential in a medium that forwards, indexes and
    /// backs itself up — and link previews in mail and chat clients FETCH urls
    /// to render them, so a state-changing GET can fire without anyone
    /// clicking. A callback button carries no capability: Telegram reports the
    /// pressing user's id, and the receiver checks it against an allowlist.
    async fn send_message_with_markup(
        &self,
        bot_token: &str,
        chat_id: &str,
        text: &str,
        parse_mode: &str,
        disable_notification: bool,
        reply_markup: Option<&serde_json::Value>,
    ) -> Result<i64> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token);

        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": parse_mode,
            "disable_notification": disable_notification,
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
            .map_err(|e| CommsError::Telegram(format!("HTTP request failed: {}", e)))?;

        let status = response.status();
        let body: TelegramResponse = response
            .json()
            .await
            .map_err(|e| CommsError::Telegram(format!("Failed to parse response: {}", e)))?;

        if body.ok {
            let message_id = body
                .result
                .map(|m| m.message_id)
                .unwrap_or(0);
            Ok(message_id)
        } else {
            let error_msg = body
                .description
                .unwrap_or_else(|| format!("HTTP {}", status));
            Err(CommsError::Telegram(error_msg))
        }
    }
}

#[async_trait]
impl NotificationChannel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    #[instrument(skip(self, message, config), fields(message_id = %message.id, chat_id = ?recipient.address.get("chat_id")))]
    async fn send(
        &self,
        message: &CommsMessage,
        recipient: &RecipientConfig,
        config: &ChannelConfig,
    ) -> Result<SendResult> {
        let telegram_config = TelegramConfig::from_channel_config(config)?;

        // Use recipient-specific chat_id if provided, otherwise use config default
        let chat_id = recipient
            .address
            .get("chat_id")
            .unwrap_or(&telegram_config.chat_id);

        info!(
            chat_id = %chat_id,
            message_type = %message.message_type,
            "Sending Telegram notification"
        );

        // Render content
        let content = self
            .render_content(message, Some(&message.message_type))
            .await?;

        // Send message, carrying an inline keyboard when the producer asked for
        // one. This is the only path that turns a notification into a decision
        // the operator can make from a phone.
        let markup = message
            .dispatch
            .as_ref()
            .and_then(|d| d.reply_markup.as_ref());

        let message_id = self
            .send_message_with_markup(
                &telegram_config.bot_token,
                chat_id,
                &content.body,
                &telegram_config.parse_mode,
                telegram_config.disable_notification,
                markup,
            )
            .await?;

        debug!(telegram_message_id = message_id, "Telegram message sent");

        Ok(SendResult::success_with_id(message_id.to_string())
            .with_metadata("chat_id", chat_id.clone()))
    }

    fn validate_config(&self, config: &ChannelConfig) -> Result<()> {
        let telegram_config = TelegramConfig::from_channel_config(config)?;

        if telegram_config.bot_token.is_empty() {
            return Err(CommsError::Validation("Bot token is required".into()));
        }
        if telegram_config.chat_id.is_empty() {
            return Err(CommsError::Validation("Chat ID is required".into()));
        }

        Ok(())
    }

    fn default_rate_limit(&self) -> RateLimit {
        // Telegram allows 30 messages/second to same chat
        RateLimit {
            max_requests: 30,
            window_secs: 1,
        }
    }

    async fn render_content(
        &self,
        message: &CommsMessage,
        template_name: Option<&str>,
    ) -> Result<RenderedContent> {
        let template_key = format!(
            "telegram/{}",
            template_name.unwrap_or(&message.message_type)
        );

        // Build template context
        let mut context = tera::Context::new();
        context.insert("message", message);
        context.insert("sender", &message.sender);
        context.insert("content", &message.content);
        context.insert("metadata", &message.metadata);
        context.insert("site_id", &message.site_id);
        context.insert("timestamp", &message.timestamp);

        // Try to render template, fall back to default
        match self.template_renderer.render(&template_key, &context).await {
            Ok(rendered) => Ok(rendered),
            Err(_) => {
                // Fall back to default format (MarkdownV2)
                let sender_name = message
                    .sender
                    .as_ref()
                    .and_then(|s| s.name.as_deref())
                    .unwrap_or("Unknown");

                let subject = message
                    .content
                    .subject
                    .as_deref()
                    .unwrap_or("Notification");

                let body = message
                    .content
                    .body
                    .as_deref()
                    .unwrap_or("No content");

                // Escape for MarkdownV2
                let escaped_subject = Self::escape_markdown_v2(subject);
                let escaped_body = Self::escape_markdown_v2(body);
                let escaped_sender = Self::escape_markdown_v2(sender_name);
                let escaped_site = Self::escape_markdown_v2(&message.site_id);

                let text = format!(
                    "*{}*\n\n{}\n\n_From: {} \\| Site: {}_",
                    escaped_subject, escaped_body, escaped_sender, escaped_site
                );

                Ok(RenderedContent::plain(text))
            }
        }
    }
}
