//! Settings data models

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::channels::{ChannelConfig, RateLimit};

/// Complete site settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteSettings {
    /// Site identifier
    pub site_id: String,

    /// Whether notifications are enabled for this site
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Channel-specific settings
    pub channels: ChannelSettings,

    /// Routing rules for message types
    #[serde(default)]
    pub routing_rules: Vec<RoutingRule>,

    /// Rate limits per channel
    #[serde(default)]
    pub rate_limits: HashMap<String, RateLimit>,

    /// Spam filter settings
    #[serde(default)]
    pub filters: FilterSettings,

    /// Retry configuration
    #[serde(default)]
    pub retry: RetrySettings,
}

fn default_true() -> bool {
    true
}

/// Channel-specific settings container
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelSettings {
    pub email: Option<ChannelConfig>,
    pub telegram: Option<ChannelConfig>,
    pub sms: Option<ChannelConfig>,
    /// Additional custom channels
    #[serde(flatten)]
    pub custom: HashMap<String, ChannelConfig>,
}

/// Routing rule for message types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    /// Message type to match (or "all")
    #[serde(rename = "type")]
    pub message_type: String,

    /// Channels to dispatch to
    pub channels: Vec<String>,

    /// Optional priority override
    pub priority_override: Option<u8>,
}

/// Filter settings (spam, blocklists, etc.)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FilterSettings {
    /// Enable spam filtering
    #[serde(default)]
    pub spam_enabled: bool,

    /// Action to take on spam
    #[serde(default = "default_spam_action")]
    pub spam_action: SpamAction,

    /// Keywords blocklist
    #[serde(default)]
    pub keywords_blocklist: Vec<String>,

    /// IP address blocklist
    #[serde(default)]
    pub ip_blocklist: Vec<String>,

    /// Email domain blocklist
    #[serde(default)]
    pub email_blocklist: Vec<String>,
}

fn default_spam_action() -> SpamAction {
    SpamAction::Flag
}

/// Action to take when spam is detected
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpamAction {
    /// Reject the message entirely
    Reject,
    /// Flag but still process
    #[default]
    Flag,
    /// Move to quarantine
    Quarantine,
}

/// Retry configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrySettings {
    /// Maximum retry attempts
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,

    /// Base delay in seconds (for exponential backoff)
    #[serde(default = "default_base_delay")]
    pub base_delay_secs: u64,

    /// Maximum delay in seconds
    #[serde(default = "default_max_delay")]
    pub max_delay_secs: u64,
}

fn default_max_attempts() -> u32 {
    5
}

fn default_base_delay() -> u64 {
    30
}

fn default_max_delay() -> u64 {
    3600
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            base_delay_secs: default_base_delay(),
            max_delay_secs: default_max_delay(),
        }
    }
}

impl SiteSettings {
    /// Create settings with defaults for a site
    pub fn new(site_id: impl Into<String>) -> Self {
        Self {
            site_id: site_id.into(),
            enabled: true,
            channels: ChannelSettings::default(),
            routing_rules: vec![],
            rate_limits: HashMap::new(),
            filters: FilterSettings::default(),
            retry: RetrySettings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::telegram::TelegramConfig;

    /// The exact shape an operator writes to {site}:comms:config to stand up
    /// an operator channel that can carry approval buttons. Asserted here
    /// against the code that consumes it, because a config that fails to
    /// deserialize takes the site's OTHER channels down with it —
    /// get_settings returns Err for the whole site, not just the bad channel.
    const OPERATOR_CHANNEL_CONFIG: &str = r#"{
      "site_id": "geodine",
      "enabled": true,
      "channels": {
        "email": {
          "enabled": true,
          "config": {},
          "recipients": [{"email": "operator@example.test"}]
        },
        "telegram": {
          "enabled": true,
          "config": {
            "bot_token": "0000000000:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "chat_id": "1234567890"
          },
          "recipients": [{"chat_id": "1234567890"}]
        }
      }
    }"#;

    #[test]
    fn operator_channel_config_deserializes() {
        let s: SiteSettings = serde_json::from_str(OPERATOR_CHANNEL_CONFIG).unwrap();
        assert_eq!(s.site_id, "geodine");
        assert!(s.enabled);
        let tg = s.channels.telegram.expect("telegram channel missing");
        assert!(tg.enabled);
        assert_eq!(tg.recipients.len(), 1);
        // The flattened address map is what bot.rs reads to pick a chat.
        assert_eq!(tg.recipients[0].address.get("chat_id").map(String::as_str), Some("1234567890"));
        // Default min_priority must admit a grant notification (priority 2).
        assert!(tg.recipients[0].should_receive("alert", 2));
    }

    #[test]
    fn operator_channel_yields_a_usable_telegram_config() {
        let s: SiteSettings = serde_json::from_str(OPERATOR_CHANNEL_CONFIG).unwrap();
        let cfg = TelegramConfig::from_channel_config(&s.channels.telegram.unwrap())
            .expect("TelegramConfig extraction failed — bot_token/chat_id shape is wrong");
        assert!(!cfg.bot_token.is_empty());
        assert_eq!(cfg.chat_id, "1234567890");
        // Grant bodies contain -, ., (, ) — escape_markdown_v2 handles them,
        // so the default parse mode is safe to inherit.
        assert_eq!(cfg.parse_mode, "MarkdownV2");
    }

    /// The custom-channel flatten means an unrecognised key under `channels`
    /// is parsed AS a ChannelConfig. A typo like "telegrm" therefore does not
    /// error — it silently becomes a custom channel nothing dispatches to.
    #[test]
    fn a_misspelled_channel_is_silently_accepted() {
        let s: SiteSettings = serde_json::from_str(
            r#"{"site_id":"x","channels":{"telegrm":{"enabled":true,"config":{},"recipients":[]}}}"#,
        )
        .unwrap();
        assert!(s.channels.telegram.is_none());
        assert!(s.channels.custom.contains_key("telegrm"));
    }
}
