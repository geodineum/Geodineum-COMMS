//! Spam filter for what visitors send through a site's forms.
//!
//! Signals add up to a score; 0.7 or more is spam. No single weak signal (a dotted address, a gibberish name, one
//! keyword) reaches it alone: a real address like `j.p.de.vries@gmail.com` is never spam for its dots.

use std::collections::HashSet;
use tracing::{debug, info};

use crate::channels::CommsMessage;
use crate::settings::{FilterSettings, SpamAction};

/// What the daemon does with a message after the filter.
#[derive(Debug, Clone)]
pub enum SpamVerdict {
    Clean,
    /// Delivered, its subject marked for review.
    Flag(SpamCheckResult),
    /// Kept in the archive as spam, not delivered.
    Quarantine(SpamCheckResult),
    /// Neither delivered nor kept.
    Drop(SpamCheckResult),
}

/// A message a visitor sent: it names an email, or comes from an address that is not this machine. Alerts, login codes,
/// receipts and test sends carry neither and are never filtered.
pub fn from_visitor(message: &CommsMessage) -> bool {
    let Some(sender) = message.sender.as_ref() else { return false };
    let email = sender.email.as_deref().map(str::trim).unwrap_or("");
    let ip = sender.ip.as_deref().map(str::trim).unwrap_or("");
    !email.is_empty() || (!ip.is_empty() && ip != "127.0.0.1" && ip != "::1")
}

/// The site's own filter when the site turned it on (its lists on top of the built-in ones, its action), otherwise the
/// daemon-wide one, whose spam is quarantined.
pub fn verdict(global: Option<&SpamFilter>, site: Option<&FilterSettings>, message: &CommsMessage) -> SpamVerdict {
    if !from_visitor(message) {
        return SpamVerdict::Clean;
    }
    if let Some(fs) = site.filter(|f| f.spam_enabled) {
        let r = SpamFilter::from_settings(fs).check(message);
        if !r.is_spam {
            return SpamVerdict::Clean;
        }
        return match fs.spam_action {
            SpamAction::Flag => SpamVerdict::Flag(r),
            SpamAction::Quarantine => SpamVerdict::Quarantine(r),
            SpamAction::Reject => SpamVerdict::Drop(r),
        };
    }
    match global.map(|g| g.check(message)) {
        Some(r) if r.is_spam => SpamVerdict::Quarantine(r),
        _ => SpamVerdict::Clean,
    }
}

const VOWELS: &str = "aeiouyáéíóúàèìòùäëïöüâêîôûãõåæøœý";

/// `word` occurs in `text` with no letter or digit on either side (`crypto` is not in `cryptographic`).
fn contains_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(pos) = text[from..].find(word) {
        let start = from + pos;
        let end = start + word.len();
        let before = text[..start].chars().next_back().map_or(true, |c| !c.is_alphanumeric());
        let after = text[end..].chars().next().map_or(true, |c| !c.is_alphanumeric());
        if before && after {
            return true;
        }
        from = start + text[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// A local part chopped into many one-letter pieces (`te.l.o.x.i.vu.jogi49`): Gmail ignores dots, so form spam varies
/// them. Five or more pieces with at least four of one character; a few dots between names never qualify.
fn dot_scattered(email: &str) -> bool {
    let local = email.split('@').next().unwrap_or("");
    let pieces: Vec<&str> = local.split('.').collect();
    pieces.len() >= 5 && pieces.iter().filter(|p| p.chars().count() == 1).count() >= 4
}

/// One long run of letters and digits with no space (`xkqzvbwplmrtyfghd`).
fn random_token(text: &str) -> bool {
    let t = text.trim();
    t.chars().count() > 15 && t.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Result of spam filtering
#[derive(Debug, Clone)]
pub struct SpamCheckResult {
    pub is_spam: bool,
    pub score: f32,
    pub reasons: Vec<String>,
}

impl SpamCheckResult {
    pub fn clean() -> Self {
        Self {
            is_spam: false,
            score: 0.0,
            reasons: vec![],
        }
    }

    pub fn spam(score: f32, reasons: Vec<String>) -> Self {
        Self {
            is_spam: true,
            score,
            reasons,
        }
    }
}

/// Basic spam filter
pub struct SpamFilter {
    keywords_blocklist: HashSet<String>,
    ip_blocklist: HashSet<String>,
    email_blocklist: HashSet<String>,
}

impl SpamFilter {
    pub fn new() -> Self {
        Self {
            keywords_blocklist: HashSet::new(),
            ip_blocklist: HashSet::new(),
            email_blocklist: HashSet::new(),
        }
    }

    /// Build from the daemon-level YAML config: the curated built-in
    /// blocklists EXTENDED by the operator-configured entries (additive by
    /// design — configuration can only strengthen filtering, never weaken
    /// the shipped baseline).
    pub fn with_extra_blocklists(keywords: &[String], ips: &[String]) -> Self {
        let mut filter = Self::default();
        filter
            .keywords_blocklist
            .extend(keywords.iter().map(|s| s.to_lowercase()));
        filter.ip_blocklist.extend(ips.iter().cloned());
        filter
    }

    /// A site's filter: the built-in lists EXTENDED by the site's own (a site can add, never remove).
    pub fn from_settings(settings: &FilterSettings) -> Self {
        let mut filter = Self::default();
        filter.keywords_blocklist.extend(settings.keywords_blocklist.iter().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()));
        filter.ip_blocklist.extend(settings.ip_blocklist.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
        filter.email_blocklist.extend(
            settings.email_blocklist.iter().map(|s| s.trim().trim_start_matches('@').to_lowercase()).filter(|s| !s.is_empty()),
        );
        filter
    }

    /// Check if a message is spam
    pub fn check(&self, message: &CommsMessage) -> SpamCheckResult {
        let mut score: f32 = 0.0;
        let mut reasons = Vec::new();

        // Check sender IP
        if let Some(ref sender) = message.sender {
            if let Some(ref ip) = sender.ip {
                if self.ip_blocklist.contains(ip) {
                    score += 1.0;
                    reasons.push(format!("IP {} in blocklist", ip));
                }
            }

            // Check sender email domain
            if let Some(ref email) = sender.email {
                let email_lower = email.trim().to_lowercase();
                if let Some(domain) = email_lower.split('@').nth(1) {
                    if self.email_blocklist.contains(domain) {
                        score += 0.8;
                        reasons.push(format!("Email domain {} in blocklist", domain));
                    }
                }
                if dot_scattered(&email_lower) {
                    score += 0.5;
                    reasons.push("Address scattered with one-letter pieces".to_string());
                }
            }

            // Check for gibberish name (high entropy, no vowels, etc.)
            if let Some(ref name) = sender.name {
                if self.looks_like_gibberish(name) {
                    score += 0.5;
                    reasons.push("Sender name appears to be gibberish".to_string());
                }
            }
        }

        // Check content for blocked keywords
        let content_text = format!(
            "{} {}",
            message.content.subject.as_deref().unwrap_or(""),
            message.content.body.as_deref().unwrap_or("")
        )
        .to_lowercase();

        for keyword in &self.keywords_blocklist {
            if contains_word(&content_text, keyword) {
                score += 0.3;
                reasons.push(format!("Contains blocked keyword: {}", keyword));
            }
        }

        // Check for gibberish content
        if let Some(ref subject) = message.content.subject {
            if self.looks_like_gibberish(subject) || random_token(subject) {
                score += 0.4;
                reasons.push("Subject appears to be gibberish".to_string());
            }
        }

        let links = message.content.body.as_deref().map_or(0, |b| b.matches("http://").count() + b.matches("https://").count());
        if links >= 3 {
            score += 0.4;
            reasons.push(format!("{} links in the message", links));
        }

        // Threshold check
        let is_spam = score >= 0.7;

        if is_spam {
            info!(
                message_id = %message.id,
                score = score,
                reasons = ?reasons,
                "Message flagged as spam"
            );
        } else if score > 0.0 {
            debug!(
                message_id = %message.id,
                score = score,
                "Message has low spam score"
            );
        }

        if is_spam {
            SpamCheckResult::spam(score, reasons)
        } else {
            SpamCheckResult::clean()
        }
    }

    /// Basic heuristic to detect gibberish text
    fn looks_like_gibberish(&self, text: &str) -> bool {
        if text.len() < 3 {
            return false;
        }

        let original = text;
        let text = text.to_lowercase();

        // Count vowels
        let vowel_count = text.chars().filter(|c| VOWELS.contains(*c)).count();
        let letter_count = text.chars().filter(|c| c.is_alphabetic()).count();

        if letter_count == 0 {
            return false;
        }

        let vowel_ratio = vowel_count as f32 / letter_count as f32;

        // Normal English has ~40% vowels; gibberish often has very low or very high
        if vowel_ratio < 0.15 || vowel_ratio > 0.65 {
            return true;
        }

        // Check for too many consecutive consonants (> 5)
        let mut consonant_run = 0;
        for c in text.chars() {
            if c.is_alphabetic() && !VOWELS.contains(c) {
                consonant_run += 1;
                if consonant_run > 5 {
                    return true;
                }
            } else {
                consonant_run = 0;
            }
        }

        // Random uppercase/lowercase mixing (`aBcDeFgH`), read on the text as written; `McDonald` and `iPhone` pass
        let letters = original.chars().filter(|c| c.is_alphabetic()).count();
        let has_mixed_case =
            original.chars().any(|c| c.is_uppercase()) && original.chars().any(|c| c.is_lowercase());

        if has_mixed_case && letters >= 8 {
            let mut case_changes = 0;
            let mut last_upper: Option<bool> = None;
            for c in original.chars() {
                if c.is_alphabetic() {
                    let is_upper = c.is_uppercase();
                    if let Some(last) = last_upper {
                        if last != is_upper {
                            case_changes += 1;
                        }
                    }
                    last_upper = Some(is_upper);
                }
            }
            // Too many case changes relative to length suggests gibberish
            if case_changes > letters / 2 {
                return true;
            }
        }

        false
    }
}

impl Default for SpamFilter {
    fn default() -> Self {
        let mut filter = Self::new();

        // Add some common spam keywords
        filter.keywords_blocklist.extend(
            [
                "viagra",
                "cialis",
                "crypto",
                "bitcoin",
                "investment opportunity",
                "make money fast",
                "nigerian prince",
                "lottery winner",
                "casino",
                "free money",
            ]
            .iter()
            .map(|s| s.to_string()),
        );

        filter
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{MessageContent, SenderInfo};
    use std::collections::HashMap;

    fn form(name: &str, email: &str, ip: &str, subject: &str, body: &str) -> CommsMessage {
        CommsMessage {
            id: "m-1".into(),
            message_type: "contact".into(),
            timestamp: "2026-11-09T12:00:00Z".into(),
            site_id: "palaciodeobras_com".into(),
            priority: 3,
            sender: Some(SenderInfo {
                name: Some(name.into()),
                email: Some(email.into()),
                phone: None,
                user_agent: None,
                ip: Some(ip.into()),
            }),
            content: MessageContent { subject: Some(subject.into()), body: Some(body.into()), attachments: HashMap::new() },
            metadata: HashMap::new(),
            environment: "production".into(),
            dispatch: None,
            to: None,
        }
    }

    fn on(action: SpamAction) -> FilterSettings {
        FilterSettings { spam_enabled: true, spam_action: action, ..Default::default() }
    }

    fn is_clean(v: SpamVerdict) -> bool {
        matches!(v, SpamVerdict::Clean)
    }

    #[test]
    fn real_people_with_dots_accents_and_short_notes_pass() {
        let site = on(SpamAction::Reject);
        for m in [
            form("Jan-Pieter de Vries", "j.p.de.vries@gmail.com", "203.0.113.7", "Offerte badkamer renovatie", "Kunt u langskomen voor een offerte?"),
            form("Niels Erik Toren", "niels.erik.toren@gmail.com", "203.0.113.8", "Question", "Call me back"),
            form("Íñigo Martínez", "inigo.martinez@empresa.es", "203.0.113.9", "Reforma de cocina", "Hola, ¿cuándo podéis venir?"),
            form("Ana McDonald", "a.b.c.mcdonald@outlook.com", "203.0.113.10", "iPhone repair", "See https://example.com/photo"),
            form("Wout", "w@x.nl", "203.0.113.11", "Cryptographic signing for our shop", "We need help with cryptographic receipts."),
        ] {
            let r = SpamFilter::from_settings(&site).check(&m);
            assert!(!r.is_spam, "flagged {:?}: {:?}", m.sender.as_ref().unwrap().email, r.reasons);
            assert!(is_clean(verdict(None, Some(&site), &m)));
        }
    }

    #[test]
    fn a_scattered_address_alone_is_not_spam_but_with_a_random_subject_it_is() {
        let site = on(SpamAction::Quarantine);
        let alone = form("Teresa", "te.l.o.x.i.vu.jogi49@gmail.com", "198.51.100.4", "Pregunta sobre precios", "¿Cuánto cuesta pintar un piso?");
        assert!(is_clean(verdict(None, Some(&site), &alone)));
        let forum_spam = form("Teresa", "te.l.o.x.i.vu.jogi49@gmail.com", "198.51.100.4", "xkqzvbwplmrtyfghd", "hi");
        assert!(matches!(verdict(None, Some(&site), &forum_spam), SpamVerdict::Quarantine(_)));
    }

    #[test]
    fn the_sites_action_decides_and_drop_is_reject() {
        let spam = form("xzqwrtkplm", "a@b.example", "198.51.100.5", "xkqzvbwplmrtyfghd", "casino casino");
        assert!(matches!(verdict(None, Some(&on(SpamAction::Flag)), &spam), SpamVerdict::Flag(_)));
        assert!(matches!(verdict(None, Some(&on(SpamAction::Reject)), &spam), SpamVerdict::Drop(_)));
        let fs: FilterSettings = serde_json::from_str(r#"{"spam_enabled":true,"spam_action":"drop"}"#).unwrap();
        assert!(matches!(fs.spam_action, SpamAction::Reject));
        let off: FilterSettings = serde_json::from_str(r#"{"spam_enabled":false}"#).unwrap();
        assert!(is_clean(verdict(None, Some(&off), &spam)), "a site that did not turn it on is not filtered by its own settings");
        assert!(matches!(verdict(Some(&SpamFilter::default()), Some(&off), &spam), SpamVerdict::Quarantine(_)), "the daemon-wide filter still applies");
    }

    #[test]
    fn a_sites_lists_add_to_the_built_in_ones() {
        let mut site = on(SpamAction::Reject);
        site.email_blocklist = vec!["@spam.example".into()];
        site.ip_blocklist = vec!["198.51.100.66".into()];
        assert!(matches!(verdict(None, Some(&site), &form("Bo", "bo@spam.example", "203.0.113.1", "Hello", "Hello there")), SpamVerdict::Drop(_)));
        assert!(matches!(verdict(None, Some(&site), &form("Bo", "bo@fine.example", "198.51.100.66", "Hello", "Hello there")), SpamVerdict::Drop(_)));
        let r = SpamFilter::from_settings(&site).check(&form("Bo", "bo@fine.example", "203.0.113.1", "Hello", "viagra, casino and free money"));
        assert!(r.is_spam && r.reasons.iter().any(|x| x.contains("viagra")), "the built-in keywords stay: {:?}", r.reasons);
        assert!(!SpamFilter::from_settings(&site).check(&form("Bo", "bo@fine.example", "203.0.113.1", "Hello", "a casino night for staff")).is_spam,
            "one keyword alone is not spam");
    }

    #[test]
    fn messages_no_visitor_sent_are_never_filtered() {
        let site = on(SpamAction::Reject);
        let mut alert = form("x", "", "", "xkqzvbwplmrtyfghd", "casino casino casino");
        alert.sender = None;
        assert!(is_clean(verdict(Some(&SpamFilter::default()), Some(&site), &alert)));
        let test_send = form("Geodineum COMMS test", "", "127.0.0.1", "xkqzvbwplmrtyfghd", "casino casino casino");
        assert!(is_clean(verdict(Some(&SpamFilter::default()), Some(&site), &test_send)));
    }

    #[test]
    fn many_links_and_random_case_count() {
        let site = on(SpamAction::Reject);
        let links = form("Bob", "bob@example.org", "203.0.113.2", "aBcDeFgHiJ", "https://a.example https://b.example http://c.example");
        assert!(matches!(verdict(None, Some(&site), &links), SpamVerdict::Drop(_)));
    }
}
