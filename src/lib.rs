//! Geodineum-COMMS: Notification Daemon for the Geodineum Constellation
//!
//! This library provides the core functionality for the Geodineum-COMMS notification daemon,
//! which processes messages from gNode comms streams and dispatches them via various
//! notification channels (email, Telegram, SMS).
//!
//! Supports bidirectional communication: outbound alerts + inbound operator interaction
//! via Telegram (and future channels) with reply-correlation, session management.

pub mod channels;
pub mod cli;
pub mod config;
pub mod consumer;
pub mod dtap;
pub mod error;
pub mod filters;
pub mod inbound;
pub mod persistence;
pub mod receipt;
pub mod retry;
pub mod router;
pub mod settings;
pub mod templates;
pub mod watch;
pub mod valkey_scan;

/// The Bot API's base URL. `COMMS_TELEGRAM_API` points it at a test server (scripts/admin-bot-proof.sh).
pub fn telegram_api() -> String {
    std::env::var("COMMS_TELEGRAM_API")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "https://api.telegram.org".to_string())
}

// Re-exports for convenience
pub use channels::{
    ChannelConfig, ChannelRegistry, CommsMessage, EmailChannel, NotificationChannel,
    RecipientConfig, SendResult, SmsChannel, TelegramChannel,
};
pub use config::Config;
pub use consumer::{SiteDiscovery, StreamConsumer};
pub use error::{CommsError, Result};
pub use filters::SpamFilter;
pub use inbound::{
    CommandAction, CommandRouter, ConversationState, PollRequest, TelegramReceiver,
    send_processing_indicator, spawn_response_poller,
};
pub use retry::RetryManager;
pub use router::MessageDispatcher;
pub use settings::{SettingsStore, SiteSettings};
pub use templates::TemplateRenderer;
pub use persistence::{MessageStore, ArchivedMessage, MessageStats};
