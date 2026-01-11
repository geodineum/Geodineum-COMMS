//! Notification channel providers
//!
//! This module contains the core `NotificationChannel` trait and implementations
//! for various notification providers (email, Telegram, SMS).

pub mod channel;
pub mod email;
pub mod sms;
pub mod telegram;

pub use channel::{
    ChannelConfig, CommsMessage, DispatchInfo, DispatchStatus, MessageContent, NotificationChannel,
    RateLimit, RecipientConfig, RenderedContent, SendResult, SenderInfo,
};
pub use email::EmailChannel;
pub use sms::SmsChannel;
pub use telegram::TelegramChannel;

use std::collections::HashMap;
use std::sync::Arc;

use crate::templates::TemplateRenderer;

/// Registry of available notification channels
pub struct ChannelRegistry {
    channels: HashMap<String, Arc<dyn NotificationChannel>>,
}

impl ChannelRegistry {
    /// Create a new channel registry with default channels
    pub fn new(template_renderer: Arc<TemplateRenderer>) -> Self {
        let mut channels: HashMap<String, Arc<dyn NotificationChannel>> = HashMap::new();

        // Register default channels
        channels.insert(
            "email".to_string(),
            Arc::new(EmailChannel::new(template_renderer.clone())),
        );
        channels.insert(
            "telegram".to_string(),
            Arc::new(TelegramChannel::new(template_renderer.clone())),
        );
        channels.insert(
            "sms".to_string(),
            Arc::new(SmsChannel::new(template_renderer)),
        );

        Self { channels }
    }

    /// Get a channel by name
    pub fn get(&self, name: &str) -> Option<Arc<dyn NotificationChannel>> {
        self.channels.get(name).cloned()
    }

    /// Register a custom channel
    pub fn register(&mut self, channel: Arc<dyn NotificationChannel>) {
        self.channels.insert(channel.name().to_string(), channel);
    }

    /// List all registered channel names
    pub fn list(&self) -> Vec<&str> {
        self.channels.keys().map(|s| s.as_str()).collect()
    }
}
