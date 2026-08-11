//! Sender-domain authorization with a known-good fallback.
//!
//! A site may configure any `from_email`, but this host can only authenticate
//! (SPF/DKIM) domains it was actually provisioned for. Sending as an
//! unprovisioned domain is how palaciodeobras.com's quote mail vanished: the
//! dispatch succeeded here and died at the recipient's DMARC. Policy:
//!
//! - A from-domain is AUTHORIZED iff it is listed in
//!   `/etc/geodineum/mail/authorized-domains` (written by
//!   `setup-mail-stack.sh` and the installer) or matches the default sender.
//! - The DEFAULT sender is `GEODINEUM_DEFAULT_FROM` (unit environment,
//!   captured at ecosystem install / `setup-mail-stack.sh --default`).
//! - Unauthorized sender + default present → rewrite the sender to the
//!   default and WARN (rate-limited) naming the service and the fix.
//! - Unauthorized sender + no default → send as-is and WARN: delivery is the
//!   operator's gamble, but mail must not silently stop.

use tracing::warn;
use std::sync::atomic::{AtomicU64, Ordering};

pub const AUTHORIZED_DOMAINS_FILE: &str = "/etc/geodineum/mail/authorized-domains";
pub const DEFAULT_FROM_ENV: &str = "GEODINEUM_DEFAULT_FROM";

pub struct ResolvedSender {
    pub from_email: String,
    /// True when the configured sender was replaced by the default.
    pub fallback_applied: bool,
}

fn domain_of(addr: &str) -> Option<String> {
    let (_, dom) = addr.rsplit_once('@')?;
    if dom.is_empty() {
        return None;
    }
    Some(dom.to_ascii_lowercase())
}

fn is_authorized(domain: &str) -> bool {
    let Ok(body) = std::fs::read_to_string(AUTHORIZED_DOMAINS_FILE) else {
        return false;
    };
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .any(|l| l.eq_ignore_ascii_case(domain))
}

fn default_from() -> Option<String> {
    std::env::var(DEFAULT_FROM_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| v.contains('@'))
}

/// One warning per 60 s window with a suppressed-count, so a form storm from
/// one misconfigured site cannot balloon the journal (the flood-control rule).
fn warn_rate_limited(text: String) {
    static LAST: AtomicU64 = AtomicU64::new(0);
    static SUPPRESSED: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let last = LAST.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= 60
        && LAST
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        let n = SUPPRESSED.swap(0, Ordering::Relaxed);
        if n > 0 {
            warn!("{} (+{} similar suppressed in the last 60s)", text, n);
        } else {
            warn!("{}", text);
        }
    } else {
        SUPPRESSED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Resolve the sender this host should actually use for `configured_from`.
pub fn resolve(service_id: &str, configured_from: &str) -> ResolvedSender {
    let Some(dom) = domain_of(configured_from) else {
        // Unparseable configured sender: the Mailbox parse downstream will
        // fail loudly; nothing useful to decide here.
        return ResolvedSender {
            from_email: configured_from.to_string(),
            fallback_applied: false,
        };
    };

    let default = default_from();
    let default_dom = default.as_deref().and_then(domain_of);

    if is_authorized(&dom) || Some(dom.as_str()) == default_dom.as_deref() {
        return ResolvedSender {
            from_email: configured_from.to_string(),
            fallback_applied: false,
        };
    }

    match default {
        Some(fallback) => {
            warn_rate_limited(format!(
                "Service: {} ({}) not authorized for sending emails — change the sender or run setup-mail-stack.sh for this domain; falling back to {}",
                service_id, dom, fallback
            ));
            ResolvedSender {
                from_email: fallback,
                fallback_applied: true,
            }
        }
        None => {
            warn_rate_limited(format!(
                "Service: {} ({}) not authorized for sending emails and no {} default is configured — sending as-is, delivery may fail at the recipient",
                service_id, dom, DEFAULT_FROM_ENV
            ));
            ResolvedSender {
                from_email: configured_from.to_string(),
                fallback_applied: false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_extraction() {
        assert_eq!(domain_of("a@b.co"), Some("b.co".to_string()));
        assert_eq!(domain_of("A@B.CO"), Some("b.co".to_string()));
        assert_eq!(domain_of("nodomain"), None);
        assert_eq!(domain_of("trailing@"), None);
    }

    #[test]
    fn unparseable_sender_passes_through() {
        let r = resolve("x", "not-an-address");
        assert_eq!(r.from_email, "not-an-address");
        assert!(!r.fallback_applied);
    }
}
