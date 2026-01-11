//! Error types for GSD-COMMS

use thiserror::Error;

/// Main error type for GSD-COMMS
#[derive(Error, Debug)]
pub enum CommsError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("ValKey connection error: {0}")]
    ValKey(#[from] redis::RedisError),

    #[error("Channel error [{channel}]: {message}")]
    Channel { channel: String, message: String },

    #[error("Email error: {0}")]
    Email(String),

    #[error("Telegram error: {0}")]
    Telegram(String),

    #[error("SMS error: {0}")]
    Sms(String),

    #[error("Template error: {0}")]
    Template(#[from] tera::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Encryption error: {0}")]
    Encryption(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Rate limit exceeded for channel: {0}")]
    RateLimited(String),

    #[error("Message not found: {0}")]
    MessageNotFound(String),

    #[error("Site not found: {0}")]
    SiteNotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result type alias for GSD-COMMS
pub type Result<T> = std::result::Result<T, CommsError>;

/// Error result for channel dispatch
#[derive(Debug)]
pub struct DispatchError {
    pub channel: String,
    pub error: CommsError,
    pub retryable: bool,
}

impl DispatchError {
    pub fn new(channel: impl Into<String>, error: CommsError, retryable: bool) -> Self {
        Self {
            channel: channel.into(),
            error,
            retryable,
        }
    }

    pub fn email(error: CommsError) -> Self {
        let retryable = matches!(
            &error,
            CommsError::ValKey(_) | CommsError::Http(_) | CommsError::Internal(_)
        );
        Self::new("email", error, retryable)
    }

    pub fn telegram(error: CommsError) -> Self {
        let retryable = matches!(&error, CommsError::Http(_) | CommsError::Internal(_));
        Self::new("telegram", error, retryable)
    }

    pub fn sms(error: CommsError) -> Self {
        let retryable = matches!(&error, CommsError::Http(_) | CommsError::Internal(_));
        Self::new("sms", error, retryable)
    }
}
