//! SMTP Email Provider using lettre

use async_trait::async_trait;
use lettre::{
    message::{header::ContentType, Mailbox, MultiPart, SinglePart},
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
        AsyncSmtpTransport,
    },
    AsyncTransport, Message,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, instrument};

use crate::channels::channel::{
    ChannelConfig, CommsMessage, NotificationChannel, RateLimit, RecipientConfig, RenderedContent,
    SendResult,
};
use crate::error::{CommsError, Result};
use crate::templates::TemplateRenderer;

/// SMTP-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_user: String,
    pub smtp_pass: String,
    pub from_email: String,
    pub from_name: String,
    pub reply_to: Option<String>,
    #[serde(default = "default_use_tls")]
    pub use_tls: bool,
}

fn default_use_tls() -> bool {
    true
}

impl SmtpConfig {
    /// Extract SmtpConfig from generic ChannelConfig
    pub fn from_channel_config(config: &ChannelConfig) -> Result<Self> {
        let config_json = serde_json::to_value(&config.config)
            .map_err(|e| CommsError::Config(format!("Failed to serialize config: {}", e)))?;

        serde_json::from_value(config_json)
            .map_err(|e| CommsError::Config(format!("Invalid SMTP config: {}", e)))
    }
}

/// Email notification channel using SMTP
pub struct EmailChannel {
    template_renderer: Arc<TemplateRenderer>,
    transport: RwLock<Option<AsyncSmtpTransport<lettre::Tokio1Executor>>>,
}

impl EmailChannel {
    pub fn new(template_renderer: Arc<TemplateRenderer>) -> Self {
        Self {
            template_renderer,
            transport: RwLock::new(None),
        }
    }

    /// Get or create the SMTP transport
    async fn get_transport(
        &self,
        smtp_config: &SmtpConfig,
    ) -> Result<AsyncSmtpTransport<lettre::Tokio1Executor>> {
        // Check if we have a cached transport
        {
            let guard = self.transport.read().await;
            if guard.is_some() {
                // For now, always create new - could add connection pooling later
            }
        }

        // Create new transport
        let creds = Credentials::new(smtp_config.smtp_user.clone(), smtp_config.smtp_pass.clone());

        let transport = if smtp_config.use_tls {
            if smtp_config.smtp_port == 465 {
                // Implicit TLS (SMTPS)
                AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(&smtp_config.smtp_host)
                    .map_err(|e| CommsError::Email(format!("Failed to create relay: {}", e)))?
                    .credentials(creds)
                    .port(smtp_config.smtp_port)
                    .build()
            } else {
                // STARTTLS (port 587)
                let tls_params = TlsParameters::new(smtp_config.smtp_host.clone())
                    .map_err(|e| CommsError::Email(format!("Failed to create TLS params: {}", e)))?;

                AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(&smtp_config.smtp_host)
                    .map_err(|e| CommsError::Email(format!("Failed to create relay: {}", e)))?
                    .credentials(creds)
                    .port(smtp_config.smtp_port)
                    .tls(Tls::Required(tls_params))
                    .build()
            }
        } else {
            // No TLS (not recommended)
            AsyncSmtpTransport::<lettre::Tokio1Executor>::builder_dangerous(&smtp_config.smtp_host)
                .credentials(creds)
                .port(smtp_config.smtp_port)
                .build()
        };

        Ok(transport)
    }

    /// Build the email message
    fn build_message(
        &self,
        smtp_config: &SmtpConfig,
        recipient_email: &str,
        content: &RenderedContent,
    ) -> Result<Message> {
        let from: Mailbox = format!("{} <{}>", smtp_config.from_name, smtp_config.from_email)
            .parse()
            .map_err(|e| CommsError::Email(format!("Invalid from address: {}", e)))?;

        let to: Mailbox = recipient_email
            .parse()
            .map_err(|e| CommsError::Email(format!("Invalid recipient address: {}", e)))?;

        let subject = content
            .subject
            .as_deref()
            .unwrap_or("Notification from GSD");

        let mut builder = Message::builder()
            .from(from)
            .to(to)
            .subject(subject);

        // Add reply-to if configured
        if let Some(ref reply_to) = smtp_config.reply_to {
            let reply_to_mailbox: Mailbox = reply_to
                .parse()
                .map_err(|e| CommsError::Email(format!("Invalid reply-to address: {}", e)))?;
            builder = builder.reply_to(reply_to_mailbox);
        }

        // Build multipart message if we have HTML
        let message = if let Some(ref html) = content.body_html {
            builder
                .multipart(
                    MultiPart::alternative()
                        .singlepart(
                            SinglePart::builder()
                                .header(ContentType::TEXT_PLAIN)
                                .body(content.body.clone()),
                        )
                        .singlepart(
                            SinglePart::builder()
                                .header(ContentType::TEXT_HTML)
                                .body(html.clone()),
                        ),
                )
                .map_err(|e| CommsError::Email(format!("Failed to build message: {}", e)))?
        } else {
            builder
                .body(content.body.clone())
                .map_err(|e| CommsError::Email(format!("Failed to build message: {}", e)))?
        };

        Ok(message)
    }
}

#[async_trait]
impl NotificationChannel for EmailChannel {
    fn name(&self) -> &str {
        "email"
    }

    #[instrument(skip(self, message, config), fields(message_id = %message.id, recipient = ?recipient.address.get("email")))]
    async fn send(
        &self,
        message: &CommsMessage,
        recipient: &RecipientConfig,
        config: &ChannelConfig,
    ) -> Result<SendResult> {
        let smtp_config = SmtpConfig::from_channel_config(config)?;

        let recipient_email = recipient
            .address
            .get("email")
            .ok_or_else(|| CommsError::Validation("Recipient email not found".into()))?;

        info!(
            recipient = %recipient_email,
            message_type = %message.message_type,
            "Sending email notification"
        );

        // Render content
        let content = self
            .render_content(message, Some(&message.message_type))
            .await?;

        // Build message
        let email_message = self.build_message(&smtp_config, recipient_email, &content)?;

        // Get transport and send
        let transport = self.get_transport(&smtp_config).await?;

        match transport.send(email_message).await {
            Ok(response) => {
                let message_id = response
                    .message()
                    .next()
                    .map(|m| m.to_string())
                    .unwrap_or_default();

                debug!(message_id = %message_id, "Email sent successfully");

                Ok(SendResult::success_with_id(message_id)
                    .with_metadata("smtp_code", response.code().to_string()))
            }
            Err(e) => {
                error!(error = %e, "Failed to send email");
                Err(CommsError::Email(format!("Send failed: {}", e)))
            }
        }
    }

    fn validate_config(&self, config: &ChannelConfig) -> Result<()> {
        let smtp_config = SmtpConfig::from_channel_config(config)?;

        if smtp_config.smtp_host.is_empty() {
            return Err(CommsError::Validation("SMTP host is required".into()));
        }
        if smtp_config.smtp_user.is_empty() {
            return Err(CommsError::Validation("SMTP user is required".into()));
        }
        if smtp_config.smtp_pass.is_empty() {
            return Err(CommsError::Validation("SMTP password is required".into()));
        }
        if smtp_config.from_email.is_empty() {
            return Err(CommsError::Validation("From email is required".into()));
        }

        // Validate recipients have email addresses
        for (i, recipient) in config.recipients.iter().enumerate() {
            if !recipient.address.contains_key("email") {
                return Err(CommsError::Validation(format!(
                    "Recipient {} missing email address",
                    i
                )));
            }
        }

        Ok(())
    }

    fn default_rate_limit(&self) -> RateLimit {
        RateLimit {
            max_requests: 100,
            window_secs: 60,
        }
    }

    async fn render_content(
        &self,
        message: &CommsMessage,
        template_name: Option<&str>,
    ) -> Result<RenderedContent> {
        let template_key = format!(
            "email/{}",
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
                // Fall back to default template
                let subject = message
                    .content
                    .subject
                    .clone()
                    .unwrap_or_else(|| format!("Notification: {}", message.message_type));

                let body = message
                    .content
                    .body
                    .clone()
                    .unwrap_or_else(|| "No content".to_string());

                let html_body = format!(
                    r#"<!DOCTYPE html>
<html>
<head><title>{}</title></head>
<body>
<h2>{}</h2>
<p>{}</p>
<hr>
<p><small>Site: {} | Type: {} | Time: {}</small></p>
</body>
</html>"#,
                    subject, subject, body, message.site_id, message.message_type, message.timestamp
                );

                Ok(RenderedContent::plain(body)
                    .with_subject(subject)
                    .with_html(html_body))
            }
        }
    }
}
