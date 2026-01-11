//! Message dispatcher - routes messages to appropriate channels

use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

use crate::channels::{ChannelConfig, ChannelRegistry, CommsMessage, NotificationChannel, RecipientConfig};
use crate::error::{CommsError, DispatchError, Result};
use crate::settings::{SiteSettings, SettingsStore};

/// Result of dispatching a message
#[derive(Debug)]
pub struct DispatchResult {
    pub message_id: String,
    pub successful_channels: Vec<String>,
    pub failed_channels: Vec<DispatchError>,
    pub skipped_channels: Vec<String>,
}

impl DispatchResult {
    pub fn is_success(&self) -> bool {
        !self.successful_channels.is_empty() && self.failed_channels.is_empty()
    }

    pub fn is_partial_success(&self) -> bool {
        !self.successful_channels.is_empty() && !self.failed_channels.is_empty()
    }

    pub fn is_failure(&self) -> bool {
        self.successful_channels.is_empty() && !self.failed_channels.is_empty()
    }

    pub fn has_retryable_failures(&self) -> bool {
        self.failed_channels.iter().any(|e| e.retryable)
    }
}

/// Dispatches messages to notification channels based on site settings
pub struct MessageDispatcher {
    channel_registry: Arc<ChannelRegistry>,
    settings_store: Arc<SettingsStore>,
}

impl MessageDispatcher {
    pub fn new(channel_registry: Arc<ChannelRegistry>, settings_store: Arc<SettingsStore>) -> Self {
        Self {
            channel_registry,
            settings_store,
        }
    }

    /// Dispatch a message to all configured channels
    #[instrument(skip(self, message), fields(message_id = %message.id, site_id = %message.site_id))]
    pub async fn dispatch(&self, message: &CommsMessage) -> Result<DispatchResult> {
        let site_id = &message.site_id;

        // Load site settings
        let settings = self
            .settings_store
            .get_settings(site_id)
            .await?
            .ok_or_else(|| CommsError::SiteNotFound(site_id.clone()))?;

        if !settings.enabled {
            info!(site_id = %site_id, "Site notifications disabled, skipping");
            return Ok(DispatchResult {
                message_id: message.id.clone(),
                successful_channels: vec![],
                failed_channels: vec![],
                skipped_channels: vec!["all (site disabled)".to_string()],
            });
        }

        // Determine which channels to use based on routing rules
        let channels = self.determine_channels(&settings, message);

        let mut result = DispatchResult {
            message_id: message.id.clone(),
            successful_channels: vec![],
            failed_channels: vec![],
            skipped_channels: vec![],
        };

        // Dispatch to each channel
        for channel_name in channels {
            match self.dispatch_to_channel(&channel_name, message, &settings).await {
                Ok(()) => {
                    result.successful_channels.push(channel_name);
                }
                Err(e) => {
                    if let CommsError::Channel { channel, message: err_msg } = &e {
                        if err_msg == "disabled" {
                            result.skipped_channels.push(channel.clone());
                            continue;
                        }
                    }
                    result.failed_channels.push(DispatchError::new(
                        channel_name,
                        e,
                        true, // Assume retryable for now
                    ));
                }
            }
        }

        // Log result
        if result.is_success() {
            info!(
                channels = ?result.successful_channels,
                "Message dispatched successfully"
            );
        } else if result.is_partial_success() {
            warn!(
                successful = ?result.successful_channels,
                failed = result.failed_channels.len(),
                "Message partially dispatched"
            );
        } else if result.is_failure() {
            error!(
                failed = result.failed_channels.len(),
                "Message dispatch failed"
            );
        }

        Ok(result)
    }

    /// Determine which channels to dispatch to based on settings and message type
    fn determine_channels(&self, settings: &SiteSettings, message: &CommsMessage) -> Vec<String> {
        // Check if message specifies channels
        if let Some(ref dispatch) = message.dispatch {
            if !dispatch.channels.is_empty() {
                return dispatch.channels.clone();
            }
        }

        // Check routing rules
        for rule in &settings.routing_rules {
            if rule.message_type == message.message_type || rule.message_type == "all" {
                return rule.channels.clone();
            }
        }

        // Default: use all enabled channels
        let mut channels = Vec::new();

        if settings.channels.email.as_ref().is_some_and(|c| c.enabled) {
            channels.push("email".to_string());
        }
        if settings.channels.telegram.as_ref().is_some_and(|c| c.enabled) {
            channels.push("telegram".to_string());
        }
        if settings.channels.sms.as_ref().is_some_and(|c| c.enabled) {
            channels.push("sms".to_string());
        }

        channels
    }

    /// Dispatch to a specific channel
    async fn dispatch_to_channel(
        &self,
        channel_name: &str,
        message: &CommsMessage,
        settings: &SiteSettings,
    ) -> Result<()> {
        // Get channel provider
        let channel = self.channel_registry.get(channel_name).ok_or_else(|| {
            CommsError::Channel {
                channel: channel_name.to_string(),
                message: "Channel not found".to_string(),
            }
        })?;

        // Get channel config
        let channel_config = self.get_channel_config(channel_name, settings)?;

        if !channel_config.enabled {
            return Err(CommsError::Channel {
                channel: channel_name.to_string(),
                message: "disabled".to_string(),
            });
        }

        // Send to each recipient
        let recipients = self.filter_recipients(&channel_config, message);

        if recipients.is_empty() {
            debug!(
                channel = %channel_name,
                message_type = %message.message_type,
                priority = message.priority,
                "No recipients match message criteria"
            );
            return Ok(());
        }

        let mut last_error = None;
        let mut success_count = 0;

        for recipient in &recipients {
            match channel.send(message, recipient, &channel_config).await {
                Ok(send_result) => {
                    debug!(
                        channel = %channel_name,
                        recipient = ?recipient.address,
                        provider_id = ?send_result.provider_id,
                        "Sent notification"
                    );
                    success_count += 1;
                }
                Err(e) => {
                    warn!(
                        channel = %channel_name,
                        recipient = ?recipient.address,
                        error = %e,
                        "Failed to send notification"
                    );
                    last_error = Some(e);
                }
            }
        }

        // If all sends failed, return the last error
        if success_count == 0 {
            if let Some(e) = last_error {
                return Err(e);
            }
        }

        Ok(())
    }

    /// Get channel configuration from settings
    fn get_channel_config(&self, channel_name: &str, settings: &SiteSettings) -> Result<ChannelConfig> {
        let config = match channel_name {
            "email" => settings.channels.email.clone(),
            "telegram" => settings.channels.telegram.clone(),
            "sms" => settings.channels.sms.clone(),
            _ => None,
        };

        config.ok_or_else(|| CommsError::Channel {
            channel: channel_name.to_string(),
            message: "Channel not configured".to_string(),
        })
    }

    /// Filter recipients based on message type and priority
    fn filter_recipients(
        &self,
        channel_config: &ChannelConfig,
        message: &CommsMessage,
    ) -> Vec<RecipientConfig> {
        channel_config
            .recipients
            .iter()
            .filter(|r| r.should_receive(&message.message_type, message.priority))
            .cloned()
            .collect()
    }
}
