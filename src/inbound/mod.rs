//! Inbound message processing for bidirectional communication
//!
//! Handles incoming operator messages from channels (Telegram, etc.),
//! routes commands to appropriate components, manages conversation state,
//! and provides SPR-compressed context for multi-turn sessions.

pub mod command_router;
pub mod conversation_state;
pub mod response_poller;
pub mod schema_registry;
pub mod telegram_receiver;
pub mod inference_chain;
pub mod workflow_dispatch;

pub use command_router::{CommandAction, CommandRouter, ParsedCommand};
pub use conversation_state::{ConvState, ContextResolution, ConversationState};
pub use response_poller::{spawn_response_poller, send_processing_indicator, PollRequest};
pub use telegram_receiver::{InlineButton, TelegramReceiver, build_inline_keyboard};

/// HTML-escape untrusted text before embedding
/// into a Telegram message sent with `parse_mode: "HTML"`. Only three chars
/// need escaping per Telegram's HTML parse-mode docs: `<`, `>`, `&`.
/// Applied to AI-generated / operator-generated text before it is wrapped
/// in markup we generate (e.g. `<i>metrics footer</i>` around the AI body).
pub fn html_escape_telegram(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            _ => out.push(c),
        }
    }
    out
}

/// Convert the Markdown subset models commonly emit into Telegram-safe HTML.
/// Code spans/blocks are pulled out first (so their content is never
/// markdown-transformed), the remainder is HTML-escaped, then only Telegram's
/// supported tags are re-introduced (`<b>`, `<code>`, `<pre>`, `<a>`). Anything
/// it can't map stays as escaped text, and the per-chunk plain fallback covers
/// any residual bad markup — so this can only improve rendering, never lose a
/// reply. Chunk BEFORE calling this (on raw text) so no tag spans a boundary.
pub fn markdown_to_telegram_html(input: &str) -> String {
    use regex::Regex;

    // 1. Vault fenced blocks and inline code behind NUL-delimited placeholders
    //    so later transforms never touch code content.
    let mut vault: Vec<String> = Vec::new();
    let fence = Regex::new(r"(?s)```[^\n]*\n?(.*?)```").unwrap();
    let mut s = fence
        .replace_all(input, |c: &regex::Captures| {
            let i = vault.len();
            // the newline before the closing ``` is a separator, not content
            let code = c[1].strip_suffix('\n').unwrap_or(&c[1]);
            vault.push(format!("<pre>{}</pre>", html_escape_telegram(code)));
            format!("\u{0}{}\u{0}", i)
        })
        .into_owned();
    let icode = Regex::new(r"`([^`\n]+)`").unwrap();
    s = icode
        .replace_all(&s, |c: &regex::Captures| {
            let i = vault.len();
            vault.push(format!("<code>{}</code>", html_escape_telegram(&c[1])));
            format!("\u{0}{}\u{0}", i)
        })
        .into_owned();

    // 2. Escape the remaining prose (placeholders — NUL + digits — pass through).
    s = html_escape_telegram(&s);

    // 3. Re-introduce Telegram's supported inline tags on the escaped text.
    s = Regex::new(r"(?m)^\s{0,3}#{1,6}\s+(.+?)\s*$")
        .unwrap()
        .replace_all(&s, "<b>$1</b>")
        .into_owned();
    s = Regex::new(r"\*\*([^*\n]+)\*\*|__([^_\n]+)__")
        .unwrap()
        .replace_all(&s, |c: &regex::Captures| {
            let inner = c
                .get(1)
                .or_else(|| c.get(2))
                .map(|m| m.as_str())
                .unwrap_or_default();
            format!("<b>{}</b>", inner)
        })
        .into_owned();
    s = Regex::new(r"\[([^\]\n]+)\]\((https?://[^)\s]+)\)")
        .unwrap()
        .replace_all(&s, "<a href=\"$2\">$1</a>")
        .into_owned();

    // 4. Restore the vaulted code.
    s = Regex::new("\u{0}(\\d+)\u{0}")
        .unwrap()
        .replace_all(&s, |c: &regex::Captures| {
            c[1].parse::<usize>()
                .ok()
                .and_then(|i| vault.get(i).cloned())
                .unwrap_or_default()
        })
        .into_owned();

    s
}

/// normalize operator-supplied strings before they
/// reach tracing / journald so CR / LF can't forge log lines or inject
/// structured-log syntax. Replaces CR with `\\r` and LF with `\\n` (literal
/// backslash-escape, not actual line-break), and truncates at 512 chars to
/// bound log size per event. Apply at every `%`-display site where the
/// value is attacker-controlled (operator_name, command text, etc.).
pub fn log_safe(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(512));
    for (i, c) in s.chars().enumerate() {
        if i >= 512 {
            out.push_str("…(truncated)");
            break;
        }
        match c {
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            _ => out.push(c),
        }
    }
    out
}
pub use inference_chain::{InferenceChainConfig, InferenceChainResult, run_inference_chain};
pub use schema_registry::publish_comms_schemas;
pub use workflow_dispatch::WorkflowDispatcher;

#[cfg(test)]
mod md_render_tests {
    use super::markdown_to_telegram_html as md;

    #[test]
    fn renders_common_markdown() {
        assert_eq!(md("**bold**"), "<b>bold</b>");
        assert_eq!(md("`x < y`"), "<code>x &lt; y</code>");
        assert_eq!(md("# Diagnosis"), "<b>Diagnosis</b>");
        assert_eq!(md("[site](https://a.com)"), "<a href=\"https://a.com\">site</a>");
        // fenced code: content escaped, wrapped in <pre>, not bold-transformed
        assert_eq!(md("```\na **b** <c>\n```"), "<pre>a **b** &lt;c&gt;</pre>");
        // raw angle brackets in prose are escaped, not treated as tags
        assert_eq!(md("a <tag> b"), "a &lt;tag&gt; b");
        // no markdown -> just escaped, unchanged text
        assert_eq!(md("plain text."), "plain text.");
    }
}
