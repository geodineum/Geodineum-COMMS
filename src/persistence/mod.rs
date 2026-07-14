//! SQLite persistence module for message archiving
//!
//! Provides per-site SQLite databases for persistent message storage.
//! ValKey remains the processing queue; SQLite is the archive.

pub mod backoff;
mod cleanup;
pub mod models;
pub mod retention;
mod store;

pub use backoff::{BackoffConfig, BackoffStats, BackoffTracker};
pub use cleanup::DbStats;
pub use models::{ArchivedMessage, ChannelResult, Content, MessageQuery, MessageStats, MessageStatus, Sender};
pub use retention::{CleanupResult, RetentionConfig, SpamRetentionPolicy};
pub use store::MessageStore;
