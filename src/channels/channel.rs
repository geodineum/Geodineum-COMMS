//! NotificationChannel trait - the core abstraction for all notification providers

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::error::Result;

/// A message from the gNode comms stream
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommsMessage {
    /// Unique message ID (used for idempotency)
    pub id: String,

    /// Message type (contact, alert, error, etc.)
    #[serde(rename = "type")]
    pub message_type: String,

    /// ISO-8601 timestamp
    pub timestamp: String,

    /// Site identifier for multi-tenancy
    pub site_id: String,

    /// Priority level (1=critical, 5=low)
    #[serde(default = "default_priority")]
    pub priority: u8,

    /// Sender information
    pub sender: Option<SenderInfo>,

    /// Message content
    pub content: MessageContent,

    /// Metadata
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,

    /// DTAP environment of the originating site. Authoritative for non-prod
    /// side-effect gating (see crate::dtap): the producer SHOULD stamp this so
    /// a non-prod message mis-routed onto the production stream is still caught.
    /// Defaults to "production" only when neither the producer nor the stream
    /// suffix supplies it — parse_message resolves it explicitly from the
    /// stream key so a real message always carries a concrete value.
    #[serde(default = "default_environment")]
    pub environment: String,

    /// Dispatch status
    pub dispatch: Option<DispatchInfo>,
}

fn default_priority() -> u8 {
    3
}

fn default_environment() -> String {
    "production".to_string()
}

/// Sender information from the comms message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenderInfo {
    pub name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
}

/// Message content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageContent {
    pub subject: Option<String>,
    pub body: Option<String>,
    /// Attachments — accepts both JSON object {} and array [] (PHP sends [] for empty)
    #[serde(default, deserialize_with = "deserialize_attachments")]
    pub attachments: HashMap<String, serde_json::Value>,
}

/// Deserialize attachments from either a JSON object or array.
/// PHP's json_encode sends empty attachments as `[]` (array) rather than `{}` (object),
/// which would fail HashMap deserialization and null out the entire MessageContent struct.
fn deserialize_attachments<'de, D>(deserializer: D) -> std::result::Result<HashMap<String, serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Deserialize;
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Object(map) => Ok(map.into_iter().collect()),
        serde_json::Value::Array(arr) => {
            let mut map = HashMap::new();
            for (i, v) in arr.into_iter().enumerate() {
                map.insert(i.to_string(), v);
            }
            Ok(map)
        }
        _ => Ok(HashMap::new()),
    }
}

/// Dispatch status information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchInfo {
    #[serde(default)]
    pub channels: Vec<String>,
    /// Defaulted because it is COMMS' own bookkeeping, not something a
    /// producer knows to supply. Without a default, `{"channels":["email"]}`
    /// fails to deserialize and parse_field_safely turns that into None — the
    /// whole dispatch block is discarded and the channel selection it asked
    /// for is silently ignored while delivery still appears to work.
    #[serde(default)]
    pub status: DispatchStatus,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub last_attempt: Option<String>,
    #[serde(default)]
    pub next_retry: Option<String>,
    /// Optional channel-native interaction affordance, currently a Telegram
    /// inline keyboard. Passed through verbatim to the channel that
    /// understands it and ignored by the ones that do not, so a notification
    /// can be ANSWERED where the medium supports answering.
    #[serde(default)]
    pub reply_markup: Option<serde_json::Value>,
}

/// Dispatch status enum
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DispatchStatus {
    Pending,
    Processing,
    Sent,
    Failed,
    Spam,
}

impl Default for DispatchStatus {
    fn default() -> Self {
        Self::Pending
    }
}

/// Result of a channel send operation
#[derive(Debug, Clone)]
pub struct SendResult {
    /// Whether the send was successful
    pub success: bool,

    /// Provider-specific message ID (e.g., SMTP message-id, Telegram message_id)
    pub provider_id: Option<String>,

    /// Additional metadata from the provider
    pub metadata: HashMap<String, String>,
}

impl SendResult {
    pub fn success() -> Self {
        Self {
            success: true,
            provider_id: None,
            metadata: HashMap::new(),
        }
    }

    pub fn success_with_id(provider_id: impl Into<String>) -> Self {
        Self {
            success: true,
            provider_id: Some(provider_id.into()),
            metadata: HashMap::new(),
        }
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// Rate limit configuration for a channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimit {
    /// Maximum requests per window
    pub max_requests: u32,

    /// Window duration in seconds
    pub window_secs: u64,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self {
            max_requests: 100,
            window_secs: 60,
        }
    }
}

/// Configuration for a notification channel (generic container)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub enabled: bool,
    pub config: HashMap<String, serde_json::Value>,
    pub recipients: Vec<RecipientConfig>,
    #[serde(default)]
    pub rate_limit: Option<RateLimit>,
}

/// Recipient configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipientConfig {
    /// Recipient address (email, phone, chat_id depending on channel)
    #[serde(flatten)]
    pub address: HashMap<String, String>,

    /// Message types this recipient wants (empty = all)
    #[serde(default)]
    pub types: Vec<String>,

    /// Minimum priority level (1-5, only receive messages with priority <= this)
    #[serde(default = "default_min_priority")]
    pub min_priority: u8,
}

fn default_min_priority() -> u8 {
    5
}

impl RecipientConfig {
    /// Check if this recipient should receive a message of the given type and priority
    pub fn should_receive(&self, message_type: &str, priority: u8) -> bool {
        // Check priority
        if priority > self.min_priority {
            return false;
        }

        // Check type filter
        if self.types.is_empty() {
            return true;
        }

        self.types.iter().any(|t| t == message_type || t == "all")
    }
}

/// The core trait for notification channels
#[async_trait]
pub trait NotificationChannel: Send + Sync {
    /// Get the channel name (e.g., "email", "telegram", "sms")
    fn name(&self) -> &str;

    /// Send a notification
    async fn send(
        &self,
        message: &CommsMessage,
        recipient: &RecipientConfig,
        config: &ChannelConfig,
    ) -> Result<SendResult>;

    /// Validate channel configuration
    fn validate_config(&self, config: &ChannelConfig) -> Result<()>;

    /// Get the default rate limit for this channel
    fn default_rate_limit(&self) -> RateLimit;

    /// Render the message content for this channel
    async fn render_content(
        &self,
        message: &CommsMessage,
        template_name: Option<&str>,
    ) -> Result<RenderedContent>;
}

/// Rendered content for a channel
#[derive(Debug, Clone)]
pub struct RenderedContent {
    pub subject: Option<String>,
    pub body: String,
    pub body_html: Option<String>,
}

impl RenderedContent {
    pub fn plain(body: impl Into<String>) -> Self {
        Self {
            subject: None,
            body: body.into(),
            body_html: None,
        }
    }

    pub fn with_subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    pub fn with_html(mut self, html: impl Into<String>) -> Self {
        self.body_html = Some(html.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A producer writes what it knows: which channels, and the buttons it
    // wants offered. It has no idea what `status` or `attempts` mean. Before
    // these fields were defaulted, this exact JSON failed to deserialize and
    // parse_field_safely turned the failure into None — the requested channel
    // list was dropped and delivery still worked, so nothing looked wrong.
    #[test]
    fn producer_minimal_dispatch_deserializes() {
        let d: DispatchInfo =
            serde_json::from_str(r#"{"channels":["email","telegram"]}"#).unwrap();
        assert_eq!(d.channels, vec!["email", "telegram"]);
        assert_eq!(d.status, DispatchStatus::Pending);
        assert!(d.reply_markup.is_none());
    }

    #[test]
    fn reply_markup_survives_deserialization() {
        let d: DispatchInfo = serde_json::from_str(
            r#"{"channels":["telegram"],"reply_markup":{"inline_keyboard":[[
                 {"text":"Approve","callback_data":"grant:approve:gr-1"}]]}}"#,
        )
        .unwrap();
        let kb = d.reply_markup.expect("reply_markup dropped");
        assert_eq!(
            kb["inline_keyboard"][0][0]["callback_data"],
            "grant:approve:gr-1"
        );
    }

    #[test]
    fn empty_dispatch_object_is_not_an_error() {
        let d: DispatchInfo = serde_json::from_str("{}").unwrap();
        assert!(d.channels.is_empty());
    }
}
