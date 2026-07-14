//! Error types for Geodineum-COMMS

use thiserror::Error;

/// strip the `for url (https://api.telegram.org/bot<TOKEN>/…)`
/// suffix that reqwest appends to its `Display` output. `reqwest::Error::without_url`
/// consumes `self`, which most log sites can't satisfy (they hold `&e`), so we
/// do the stripping at the string level. `api_base` is built as
/// `https://api.telegram.org/bot{bot_token}` in telegram_receiver.rs and
/// response_poller.rs, and every reqwest error derived from those clients
/// carries the bot token in the URL → token leaks to journald / log
/// aggregators without this scrub. The free function is also applied at the
/// handful of direct-reqwest error log sites that don't flow through
/// CommsError.
pub fn scrub_reqwest_url(e: &reqwest::Error) -> String {
    let s = e.to_string();
    // reqwest 0.11 Display appends " for url (<url>)" when a URL is attached.
    // Older versions used " for url: <url>". Strip either, preserving the
    // error kind / status for diagnosis.
    if let Some(idx) = s.find(" for url") {
        format!("{} (url redacted)", &s[..idx])
    } else {
        s
    }
}

/// Main error type for Geodineum-COMMS
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

    #[error("HTTP error: {}", scrub_reqwest_url(_0))]
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

/// Result type alias for Geodineum-COMMS
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
