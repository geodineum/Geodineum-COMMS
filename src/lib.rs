//! GSD-COMMS: Notification Daemon for GSD
//!
//! This library provides the core functionality for the GSD-COMMS notification daemon,
//! which processes messages from GSD comms streams and dispatches them via various
//! notification channels (email, Telegram, SMS).

pub mod api;
pub mod channels;
pub mod config;
pub mod consumer;
pub mod error;
pub mod filters;
pub mod retry;
pub mod router;
pub mod settings;
pub mod templates;

// Re-exports for convenience
pub use channels::{
    ChannelConfig, ChannelRegistry, CommsMessage, EmailChannel, NotificationChannel,
    RecipientConfig, SendResult, SmsChannel, TelegramChannel,
};
pub use config::Config;
pub use consumer::{SiteDiscovery, StreamConsumer};
pub use error::{CommsError, Result};
pub use filters::SpamFilter;
pub use retry::RetryManager;
pub use router::MessageDispatcher;
pub use settings::{SettingsStore, SiteSettings};
pub use templates::TemplateRenderer;
