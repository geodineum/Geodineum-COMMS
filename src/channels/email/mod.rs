//! Email channel module

mod sender_policy;
mod smtp;
pub use smtp::message_id_for;

pub use smtp::{EmailChannel, SmtpConfig};
