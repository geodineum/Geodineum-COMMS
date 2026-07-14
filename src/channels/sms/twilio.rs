//! Twilio SMS Provider using REST API

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

/// Twilio-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwilioConfig {
    pub account_sid: String,
    pub auth_token: String,
    pub from_number: String,
    #[serde(default)]
    pub messaging_service_sid: Option<String>,
}

impl TwilioConfig {
    /// Extract TwilioConfig from generic ChannelConfig
    pub fn from_channel_config(config: &ChannelConfig) -> Result<Self> {
        let config_json = serde_json::to_value(&config.config)
            .map_err(|e| CommsError::Config(format!("Failed to serialize config: {}", e)))?;

        serde_json::from_value(config_json)
            .map_err(|e| CommsError::Config(format!("Invalid Twilio config: {}", e)))
    }
}

/// Twilio API response
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // serde shape — fields populated from API JSON for Debug logging
struct TwilioResponse {
    sid: Option<String>,
    status: Option<String>,
    error_code: Option<i32>,
    error_message: Option<String>,
}

/// SMS notification channel using Twilio
pub struct SmsChannel {
    client: Client,
    template_renderer: Arc<TemplateRenderer>,
}

impl SmsChannel {
    pub fn new(template_renderer: Arc<TemplateRenderer>) -> Self {
        Self {
            client: Client::new(),
            template_renderer,
        }
    }

    /// Send SMS via Twilio REST API
    async fn send_sms(
        &self,
        config: &TwilioConfig,
        to_number: &str,
        body: &str,
    ) -> Result<String> {
        let url = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json",
            config.account_sid
        );

        let mut params = vec![
            ("To", to_number.to_string()),
            ("Body", body.to_string()),
        ];

        // Use messaging service SID or from number
        if let Some(ref msid) = config.messaging_service_sid {
            params.push(("MessagingServiceSid", msid.clone()));
        } else {
            params.push(("From", config.from_number.clone()));
        }

        let response = self
            .client
            .post(&url)
            .basic_auth(&config.account_sid, Some(&config.auth_token))
            .form(&params)
            .send()
            .await
            .map_err(|e| CommsError::Sms(format!("HTTP request failed: {}", e)))?;

        let status = response.status();
        let body: TwilioResponse = response
            .json()
            .await
            .map_err(|e| CommsError::Sms(format!("Failed to parse response: {}", e)))?;

        if status.is_success() {
            let sid = body.sid.unwrap_or_else(|| "unknown".to_string());
            Ok(sid)
        } else {
            let error_msg = body.error_message.unwrap_or_else(|| {
                format!(
                    "HTTP {} - Error code: {:?}",
                    status,
                    body.error_code
                )
            });
            Err(CommsError::Sms(error_msg))
        }
    }

    /// Normalize phone number to E.164 format
    fn normalize_phone_number(phone: &str) -> String {
        let cleaned: String = phone
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '+')
            .collect();

        if cleaned.starts_with('+') {
            cleaned
        } else if cleaned.len() == 10 {
            // Assume US number
            format!("+1{}", cleaned)
        } else if cleaned.len() == 11 && cleaned.starts_with('1') {
            format!("+{}", cleaned)
        } else {
            // Return as-is, let Twilio validate
            format!("+{}", cleaned)
        }
    }
}

#[async_trait]
impl NotificationChannel for SmsChannel {
    fn name(&self) -> &str {
        "sms"
    }

    #[instrument(skip(self, message, config), fields(message_id = %message.id, phone = ?recipient.address.get("phone")))]
    async fn send(
        &self,
        message: &CommsMessage,
        recipient: &RecipientConfig,
        config: &ChannelConfig,
    ) -> Result<SendResult> {
        let twilio_config = TwilioConfig::from_channel_config(config)?;

        let phone = recipient
            .address
            .get("phone")
            .ok_or_else(|| CommsError::Validation("Recipient phone not found".into()))?;

        let normalized_phone = Self::normalize_phone_number(phone);

        info!(
            phone = %normalized_phone,
            message_type = %message.message_type,
            "Sending SMS notification"
        );

        // Render content
        let content = self
            .render_content(message, Some(&message.message_type))
            .await?;

        // SMS has a 1600 char limit, truncate if needed
        let sms_body = if content.body.len() > 1600 {
            format!("{}...", &content.body[..1597])
        } else {
            content.body.clone()
        };

        // Send SMS
        let message_sid = self
            .send_sms(&twilio_config, &normalized_phone, &sms_body)
            .await?;

        debug!(twilio_sid = %message_sid, "SMS sent successfully");

        Ok(SendResult::success_with_id(message_sid)
            .with_metadata("phone", normalized_phone))
    }

    fn validate_config(&self, config: &ChannelConfig) -> Result<()> {
        let twilio_config = TwilioConfig::from_channel_config(config)?;

        if twilio_config.account_sid.is_empty() {
            return Err(CommsError::Validation("Account SID is required".into()));
        }
        if twilio_config.auth_token.is_empty() {
            return Err(CommsError::Validation("Auth token is required".into()));
        }
        if twilio_config.from_number.is_empty() && twilio_config.messaging_service_sid.is_none() {
            return Err(CommsError::Validation(
                "Either from_number or messaging_service_sid is required".into(),
            ));
        }

        // Validate recipients have phone numbers
        for (i, recipient) in config.recipients.iter().enumerate() {
            if !recipient.address.contains_key("phone") {
                return Err(CommsError::Validation(format!(
                    "Recipient {} missing phone number",
                    i
                )));
            }
        }

        Ok(())
    }

    fn default_rate_limit(&self) -> RateLimit {
        // Conservative limit for cost control
        RateLimit {
            max_requests: 10,
            window_secs: 60,
        }
    }

    async fn render_content(
        &self,
        message: &CommsMessage,
        template_name: Option<&str>,
    ) -> Result<RenderedContent> {
        let template_key = format!(
            "sms/{}",
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
                // Fall back to concise SMS format
                let sender_name = message
                    .sender
                    .as_ref()
                    .and_then(|s| s.name.as_deref())
                    .unwrap_or("Unknown");

                let subject = message
                    .content
                    .subject
                    .as_deref()
                    .unwrap_or("Alert");

                let body = message
                    .content
                    .body
                    .as_deref()
                    .map(|b| {
                        // Truncate body for SMS
                        if b.len() > 100 {
                            format!("{}...", &b[..97])
                        } else {
                            b.to_string()
                        }
                    })
                    .unwrap_or_default();

                let text = format!(
                    "[{}] {}: {} - {}",
                    message.site_id, subject, body, sender_name
                );

                Ok(RenderedContent::plain(text))
            }
        }
    }
}
