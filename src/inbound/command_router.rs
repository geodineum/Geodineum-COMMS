//! Command router for the Geodineum bot: what an operator's message or button press does.
//!
//!   1. Button presses → Callback (polls here; grant presses are read by `geodineum grants watch`)
//!   2. Slash commands → /start, /status, /help; anything else is answered with where conversations live
//!   3. Reply commands (QUARANTINE, RETRY, DISMISS, …) → the component whose alert asked, or a workflow intent
//!   4. Anything else → told where conversations live (GeodineBot, in Geodine)

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::inbound::conversation_state::{ContextResolution, ConversationState};
use crate::inbound::telegram_receiver::InboundMessage;
use crate::inbound::workflow_dispatch::{WorkflowDispatcher, WorkflowIntent};

/// Parsed command from operator text
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedCommand {
    /// Raw command verb (e.g., "status", "quarantine")
    pub command: String,
    /// Parameters
    pub params: Vec<String>,
    /// Whether this was a slash command
    pub is_slash: bool,
    /// Whether this looks like a reply-command (single uppercase word)
    pub is_reply_command: bool,
}

/// Action to take after routing a command
#[derive(Debug)]
pub enum CommandAction {
    /// Answered by COMMS itself: `status`, `help`, or `not_here` (no conversations on this bot)
    Local { command: String },
    /// Route reply to a component via callback stream
    ComponentReply { resolution: ContextResolution },
    /// /start
    Start,
    /// Callback query from inline keyboard button press
    Callback {
        /// callback_data string (e.g., "poll:<site>:<ctx>:<i>", "grant:approve:<id>")
        data: String,
        /// Callback query ID (for answering)
        query_id: String,
        /// Message ID that had the keyboard (for editing)
        message_id: Option<i64>,
    },
    /// Dispatch a workflow to the workflow engine
    WorkflowDispatch { intent: WorkflowIntent },
    /// Unknown or empty — no action
    NoOp { reason: String },
}

/// Routes inbound operator messages to appropriate handlers
pub struct CommandRouter {
    workflow_dispatcher: Option<WorkflowDispatcher>,
}

impl Default for CommandRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandRouter {
    pub fn new() -> Self {
        Self { workflow_dispatcher: None }
    }

    /// Enable workflow dispatch with the given dispatcher
    pub fn with_workflow_dispatcher(mut self, dispatcher: WorkflowDispatcher) -> Self {
        self.workflow_dispatcher = Some(dispatcher);
        self
    }

    /// Route an inbound message to the appropriate action.
    pub async fn route_command(
        &self,
        msg: &InboundMessage,
        conv_state: &mut ConversationState,
        site_id: &str,
    ) -> Result<CommandAction> {
        if msg.is_callback {
            return Ok(CommandAction::Callback {
                data: msg.text.clone(), // callback_data is stored in text field
                query_id: msg.callback_query_id.clone(),
                message_id: msg.callback_message_id,
            });
        }

        let parsed = self.parse_text(&msg.text);
        if parsed.is_slash {
            return Ok(match parsed.command.as_str() {
                "start" => CommandAction::Start,
                "status" | "health" | "help" => CommandAction::Local { command: parsed.command.clone() },
                _ => CommandAction::Local { command: "not_here".to_string() },
            });
        }

        if parsed.is_reply_command {
            if let Some(resolution) = conv_state.resolve_reply(&msg.chat_id, site_id, &msg.text).await? {
                return Ok(CommandAction::ComponentReply { resolution });
            }
            if let Some(ref dispatcher) = self.workflow_dispatcher {
                if let Some(intent) = dispatcher.recognize_intent(&msg.text) {
                    return Ok(CommandAction::WorkflowDispatch { intent });
                }
            }
        }

        Ok(CommandAction::Local { command: "not_here".to_string() })
    }

    /// Parse operator text into a structured command
    pub fn parse_text(&self, text: &str) -> ParsedCommand {
        let trimmed = text.trim();

        if let Some(without_slash) = trimmed.strip_prefix('/') {
            let mut parts = without_slash.splitn(2, char::is_whitespace);
            let command = parts.next().unwrap_or("").to_lowercase();
            let params: Vec<String> = parts
                .next()
                .map(|rest| rest.split_whitespace().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            return ParsedCommand { command, params, is_slash: true, is_reply_command: false };
        }

        if Self::looks_like_reply_command(trimmed) {
            let mut parts = trimmed.splitn(2, char::is_whitespace);
            let command = parts.next().unwrap_or("").to_uppercase();
            let params: Vec<String> = parts
                .next()
                .map(|rest| rest.split_whitespace().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            return ParsedCommand { command, params, is_slash: false, is_reply_command: true };
        }

        ParsedCommand { command: String::new(), params: vec![], is_slash: false, is_reply_command: false }
    }

    /// Check if text looks like a reply command (uppercase action word)
    fn looks_like_reply_command(text: &str) -> bool {
        let first_word = text.split_whitespace().next().unwrap_or("");
        first_word.len() >= 3
            && first_word.chars().all(|c| c.is_ascii_uppercase() || c == '_')
            && text.split_whitespace().count() <= 4
    }
}
