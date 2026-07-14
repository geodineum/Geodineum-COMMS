//! SPR (Sparse Priming Representation) engine
//!
//! Ported from the legacy Telegram bot's SubjectBasedSPR (func/enhanced_spr.py).
//! Compresses long conversation histories into dense, topic-clustered context
//! using keyword-based topic extraction, entity recognition, and relevance scoring.

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::debug;

/// A conversation message for SPR processing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvMessage {
    pub role: String,
    pub content: String,
    pub timestamp: String,
    /// Pre-computed SPR summary (if previously compressed)
    pub spr_summary: Option<String>,
}

/// A scored message with relevance metadata
#[derive(Debug)]
struct ScoredMessage {
    content: String,
    spr_summary: Option<String>,
    score: f64,
    topics: Vec<String>,
}

/// Topic-clustered SPR compression engine.
///
/// Algorithm (from the legacy bot):
///   1. Extract topics from current message using keyword clusters
///   2. Extract key entities (PascalCase, snake_case, file paths, quoted strings)
///   3. Score each history message:
///        topic_overlap × 2.0 + entity_overlap × 1.5 + recency × 0.5
///   4. Take top-K scoring messages
///   5. Group by primary topic, format with topic headers
pub struct SPREngine {
    topic_keywords: HashMap<&'static str, Vec<&'static str>>,
    /// Compiled regexes for entity extraction
    pascal_re: Regex,
    snake_re: Regex,
    path_re: Regex,
    quote_re: Regex,
}

impl SPREngine {
    pub fn new() -> Self {
        // Topic keyword clusters — direct port from the legacy bot's enhanced_spr.py
        let mut topic_keywords: HashMap<&str, Vec<&str>> = HashMap::new();

        topic_keywords.insert(
            "technical",
            vec![
                "code", "bug", "error", "fix", "implement", "function", "api", "database", "server",
            ],
        );
        topic_keywords.insert(
            "configuration",
            vec![
                "config", "setup", "install", "settings", "environment", "path", "file",
            ],
        );
        topic_keywords.insert(
            "ai_models",
            vec![
                "model", "llm", "ollama", "gpt", "claude", "training", "prompt", "temperature",
            ],
        );
        topic_keywords.insert(
            "conversation",
            vec![
                "chat", "message", "history", "branch", "context", "conversation", "reply",
            ],
        );
        topic_keywords.insert(
            "performance",
            vec![
                "speed", "optimize", "slow", "fast", "performance", "efficiency", "cache",
            ],
        );
        topic_keywords.insert(
            "security",
            vec![
                "auth", "token", "password", "secure", "encrypt", "permission", "access",
            ],
        );
        topic_keywords.insert(
            "data",
            vec![
                "data", "database", "query", "table", "schema", "migration", "backup",
            ],
        );
        topic_keywords.insert(
            "operations",
            vec![
                "deploy", "service", "restart", "update", "monitor", "log", "alert", "health",
            ],
        );
        topic_keywords.insert(
            "general",
            vec![
                "help", "what", "how", "why", "explain", "tell", "show", "about",
            ],
        );

        Self {
            topic_keywords,
            pascal_re: Regex::new(r"\b[A-Z][a-z]+(?:[A-Z][a-z]+)+\b").unwrap(),
            snake_re: Regex::new(r"\b[a-z]+(?:_[a-z]+)+\b").unwrap(),
            path_re: Regex::new(r"[./\w]+\.[a-z]+").unwrap(),
            quote_re: Regex::new(r#""([^"]+)"|'([^']+)'"#).unwrap(),
        }
    }

    /// Extract topics from text based on keyword matching.
    /// Returns list of matching topic names, or ["general"] if none match.
    pub fn extract_topics(&self, text: &str) -> Vec<String> {
        let text_lower = text.to_lowercase();
        let mut detected = Vec::new();

        for (topic, keywords) in &self.topic_keywords {
            if keywords.iter().any(|kw| text_lower.contains(kw)) {
                detected.push(topic.to_string());
            }
        }

        if detected.is_empty() {
            detected.push("general".to_string());
        }

        detected
    }

    /// Extract key entities from text using regex patterns.
    /// Captures PascalCase terms, snake_case terms, file paths, and quoted strings.
    pub fn extract_entities(&self, text: &str) -> HashSet<String> {
        let mut entities = HashSet::new();

        // PascalCase terms (e.g., StreamConsumer, MessageDispatcher)
        for m in self.pascal_re.find_iter(text) {
            entities.insert(m.as_str().to_string());
        }

        // snake_case terms (e.g., site_id, chat_id)
        for m in self.snake_re.find_iter(text) {
            entities.insert(m.as_str().to_string());
        }

        // File paths (e.g., src/main.rs, ./config.yaml)
        for m in self.path_re.find_iter(text) {
            entities.insert(m.as_str().to_string());
        }

        // Quoted strings
        for caps in self.quote_re.captures_iter(text) {
            if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
                entities.insert(m.as_str().to_string());
            }
        }

        entities
    }

    /// Compress conversation history into dense, topic-clustered context.
    ///
    /// Scoring formula (from the legacy bot):
    ///   score = topic_overlap × 2.0 + entity_overlap × 1.5 + recency × 0.5
    ///
    /// Returns formatted context string with topic headers.
    pub fn compress(&self, messages: &[ConvMessage], current_text: &str, max_messages: usize) -> String {
        if messages.is_empty() {
            return String::new();
        }

        let current_topics = self.extract_topics(current_text);
        let current_entities = self.extract_entities(current_text);
        let current_topic_set: HashSet<&str> =
            current_topics.iter().map(|s| s.as_str()).collect();

        let now = Utc::now();

        // Score each message by relevance
        let mut scored: Vec<ScoredMessage> = messages
            .iter()
            .filter_map(|msg| {
                let combined = format!(
                    "{} {}",
                    msg.content,
                    msg.spr_summary.as_deref().unwrap_or("")
                );

                let msg_topics = self.extract_topics(&combined);
                let msg_entities = self.extract_entities(&combined);

                // Topic overlap scoring
                let topic_overlap = msg_topics
                    .iter()
                    .filter(|t| current_topic_set.contains(t.as_str()))
                    .count() as f64;

                // Entity overlap scoring
                let entity_overlap = current_entities
                    .intersection(&msg_entities)
                    .count() as f64;

                // Recency scoring (newer = higher, decay over 30 days)
                let recency = if let Ok(ts) = DateTime::parse_from_rfc3339(&msg.timestamp) {
                    let age_days = (now - ts.with_timezone(&Utc)).num_days() as f64;
                    (1.0 - (age_days / 30.0)).max(0.0)
                } else {
                    0.5 // Default for unparseable timestamps
                };

                let score = topic_overlap * 2.0 + entity_overlap * 1.5 + recency * 0.5;

                if score > 0.0 {
                    Some(ScoredMessage {
                        content: msg.content.clone(),
                        spr_summary: msg.spr_summary.clone(),
                        score,
                        topics: msg_topics,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Sort by score descending
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(max_messages);

        // Group by primary topic for organized output
        let mut topic_groups: HashMap<String, Vec<&ScoredMessage>> = HashMap::new();
        for msg in &scored {
            let primary = msg.topics.first().cloned().unwrap_or_else(|| "general".into());
            topic_groups.entry(primary).or_default().push(msg);
        }

        // Build context string with topic headers (max 3 per topic)
        let mut output = String::new();
        for (topic, msgs) in &topic_groups {
            let topic_title = topic.replace('_', " ");
            output.push_str(&format!(
                "[Context: {} related history]\n",
                capitalize_first(&topic_title)
            ));

            for msg in msgs.iter().take(3) {
                let text = msg
                    .spr_summary
                    .as_deref()
                    .unwrap_or(&msg.content);
                // Truncate very long entries
                let truncated = if text.len() > 500 {
                    format!("{}...", &text[..497])
                } else {
                    text.to_string()
                };
                output.push_str(&truncated);
                output.push('\n');
            }
            output.push('\n');
        }

        debug!(
            topics = ?current_topics,
            scored_count = scored.len(),
            "SPR compression complete"
        );

        output.trim().to_string()
    }
}

/// Capitalize the first letter of a string
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_topics() {
        let engine = SPREngine::new();

        let topics = engine.extract_topics("There is a bug in the API server code");
        assert!(topics.contains(&"technical".to_string()));

        let topics = engine.extract_topics("configure the environment settings");
        assert!(topics.contains(&"configuration".to_string()));

        let topics = engine.extract_topics("hello world");
        assert!(topics.contains(&"general".to_string()));
    }

    #[test]
    fn test_extract_entities() {
        let engine = SPREngine::new();

        let entities = engine.extract_entities("The StreamConsumer reads from site_id via src/main.rs");
        assert!(entities.contains("StreamConsumer"));
        assert!(entities.contains("site_id"));
        assert!(entities.contains("src/main.rs"));
    }

    #[test]
    fn test_compress_empty() {
        let engine = SPREngine::new();
        assert!(engine.compress(&[], "test", 10).is_empty());
    }

    #[test]
    fn test_compress_scores_relevance() {
        let engine = SPREngine::new();
        let now = Utc::now().to_rfc3339();

        let messages = vec![
            ConvMessage {
                role: "user".into(),
                content: "How do I fix the database migration error?".into(),
                timestamp: now.clone(),
                spr_summary: None,
            },
            ConvMessage {
                role: "assistant".into(),
                content: "The weather is nice today".into(),
                timestamp: now.clone(),
                spr_summary: None,
            },
        ];

        // With max_messages=1, only the highest-scoring message should appear
        let result = engine.compress(&messages, "database error fix", 1);
        // The database-related message should win (topic overlap: technical+data)
        assert!(result.contains("database migration error"));
        assert!(!result.contains("weather"));
    }
}
