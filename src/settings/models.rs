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
