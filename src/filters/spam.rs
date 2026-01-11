//! Basic spam filter implementation

use std::collections::HashSet;
use tracing::{debug, info};

use crate::channels::CommsMessage;
use crate::settings::FilterSettings;

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

    pub fn from_settings(settings: &FilterSettings) -> Self {
        Self {
            keywords_blocklist: settings
                .keywords_blocklist
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
            ip_blocklist: settings.ip_blocklist.iter().cloned().collect(),
            email_blocklist: settings
                .email_blocklist
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
        }
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
                let email_lower = email.to_lowercase();
                if let Some(domain) = email_lower.split('@').nth(1) {
                    if self.email_blocklist.contains(domain) {
                        score += 0.8;
                        reasons.push(format!("Email domain {} in blocklist", domain));
                    }
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
            if content_text.contains(keyword) {
                score += 0.3;
                reasons.push(format!("Contains blocked keyword: {}", keyword));
            }
        }

        // Check for gibberish content
        if let Some(ref subject) = message.content.subject {
            if self.looks_like_gibberish(subject) {
                score += 0.4;
                reasons.push("Subject appears to be gibberish".to_string());
            }
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

        let text = text.to_lowercase();

        // Count vowels
        let vowel_count = text.chars().filter(|c| "aeiou".contains(*c)).count();
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
            if c.is_alphabetic() && !"aeiou".contains(c) {
                consonant_run += 1;
                if consonant_run > 5 {
                    return true;
                }
            } else {
                consonant_run = 0;
            }
        }

        // Check for random uppercase/lowercase mixing
        let has_mixed_case =
            text.chars().any(|c| c.is_uppercase()) && text.chars().any(|c| c.is_lowercase());

        if has_mixed_case {
            let mut case_changes = 0;
            let mut last_upper: Option<bool> = None;
            for c in text.chars() {
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
            if case_changes > text.len() / 3 {
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
