//! CLI command handlers
//!
//! Provides command-line interface for querying messages, stats, and sites.

mod output;

use crate::persistence::{MessageQuery, MessageStats, MessageStatus, MessageStore};
use anyhow::Result;

pub use output::{OutputFormat, TablePrinter};

/// List all sites with messages
pub fn list_sites(store: &MessageStore) -> Result<Vec<String>> {
    store.list_sites().map_err(Into::into)
}

/// List messages for a site
pub fn list_messages(
    store: &MessageStore,
    site_id: Option<&str>,
    status: Option<&str>,
    message_type: Option<&str>,
    limit: usize,
) -> Result<Vec<crate::persistence::ArchivedMessage>> {
    let sites = if let Some(id) = site_id {
        vec![id.to_string()]
    } else {
        store.list_sites()?
    };

    let query = MessageQuery {
        status: status.map(MessageStatus::from_str),
        message_type: message_type.map(|s| s.to_string()),
        since: None,
        until: None,
        limit,
        offset: 0,
    };

    let mut all_messages = Vec::new();

    for site in sites {
        let messages = store.list_messages(&site, &query)?;
        all_messages.extend(messages);
    }

    // Sort by received_at descending
    all_messages.sort_by(|a, b| b.received_at.cmp(&a.received_at));

    // Limit total results
    all_messages.truncate(limit);

    Ok(all_messages)
}

/// Get stats for a site or all sites
pub fn get_stats(
    store: &MessageStore,
    site_id: Option<&str>,
) -> Result<Vec<(String, MessageStats)>> {
    let sites = if let Some(id) = site_id {
        vec![id.to_string()]
    } else {
        store.list_sites()?
    };

    let mut all_stats = Vec::new();

    for site in sites {
        let stats = store.get_stats(&site)?;
        all_stats.push((site, stats));
    }

    Ok(all_stats)
}

/// Get a specific message
pub fn get_message(
    store: &MessageStore,
    site_id: &str,
    message_id: &str,
) -> Result<Option<crate::persistence::ArchivedMessage>> {
    store.get_message(site_id, message_id).map_err(Into::into)
}

/// Retry a failed message
pub fn retry_message(store: &MessageStore, site_id: &str, message_id: &str) -> Result<bool> {
    // Check if message exists and is in a retryable state
    if let Some(msg) = store.get_message(site_id, message_id)? {
        match msg.status {
            MessageStatus::Failed | MessageStatus::PartialSent => {
                store.update_status(site_id, message_id, MessageStatus::Pending)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    } else {
        Ok(false)
    }
}
