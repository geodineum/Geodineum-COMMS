//! Persistence data models

use serde::{Deserialize, Serialize};

/// Message status in the archive
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    /// Received but not yet processed
    Pending,
    /// Currently being processed
    Processing,
    /// Successfully sent to all channels
    Sent,
    /// Partially sent (some channels failed)
    PartialSent,
    /// All channels failed
    Failed,
    /// Marked as spam
    Spam,
    /// Skipped (disabled or filtered)
    Skipped,
}

impl MessageStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Sent => "sent",
            Self::PartialSent => "partial_sent",
            Self::Failed => "failed",
            Self::Spam => "spam",
            Self::Skipped => "skipped",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "processing" => Self::Processing,
            "sent" => Self::Sent,
            "partial_sent" => Self::PartialSent,
            "failed" => Self::Failed,
            "spam" => Self::Spam,
            "skipped" => Self::Skipped,
            _ => Self::Pending,
        }
    }
}

/// Result of sending to a single channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelResult {
    pub channel: String,
    pub success: bool,
    pub provider_id: Option<String>,
    pub error: Option<String>,
    pub sent_at: Option<String>,
}

/// Archived message with full history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedMessage {
    /// Unique message ID (from stream)
    pub id: String,

    /// Stream entry ID
    pub stream_id: String,

    /// Site identifier
    pub site_id: String,

    /// Environment (production/staging)
    pub environment: String,

    /// Message type (contact, alert, error, etc.)
    pub message_type: String,

    /// Priority (1-5)
    pub priority: u8,

    /// Sender information (JSON)
    pub sender_json: String,

    /// Content (JSON)
    pub content_json: String,

    /// Metadata (JSON)
    pub metadata_json: String,

    /// Current status
    pub status: MessageStatus,

    /// Number of dispatch attempts
    pub attempts: u32,

    /// Channel results (JSON array)
    pub channel_results_json: String,

    /// Spam score (0.0-1.0, if checked)
    pub spam_score: Option<f64>,

    /// When message was received
    pub received_at: String,

    /// When message was last updated
    pub updated_at: String,

    /// When message was fully processed (sent/failed)
    pub completed_at: Option<String>,
}

impl ArchivedMessage {
    /// Get sender as parsed struct
    pub fn sender(&self) -> Option<Sender> {
        serde_json::from_str(&self.sender_json).ok()
    }

    /// Get content as parsed struct
    pub fn content(&self) -> Option<Content> {
        serde_json::from_str(&self.content_json).ok()
    }

    /// Get channel results
    pub fn channel_results(&self) -> Vec<ChannelResult> {
        serde_json::from_str(&self.channel_results_json).unwrap_or_default()
    }
}

/// Sender information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sender {
    pub name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

/// Message content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    pub subject: Option<String>,
    pub body: Option<String>,
}

/// Query parameters for listing messages
#[derive(Debug, Clone, Default)]
pub struct MessageQuery {
    pub status: Option<MessageStatus>,
    pub message_type: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

/// Statistics for a site
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageStats {
    pub total: u64,
    pub pending: u64,
    pub sent: u64,
    pub partial_sent: u64,
    pub failed: u64,
    pub spam: u64,
    pub sent_today: u64,
    pub sent_this_week: u64,
    pub by_channel: ChannelStats,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelStats {
    pub email_sent: u64,
    pub email_failed: u64,
    pub telegram_sent: u64,
    pub telegram_failed: u64,
    pub sms_sent: u64,
    pub sms_failed: u64,
}
