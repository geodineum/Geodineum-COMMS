//! Email channel module

mod sender_policy;
mod smtp;

pub use smtp::{EmailChannel, SmtpConfig};
