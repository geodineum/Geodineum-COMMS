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
