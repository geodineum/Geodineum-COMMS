//! Command router — parses operator text into structured commands and routes them
//!
//! Supports three input modes:
//!   1. Slash commands (/status, /pipeline, /reset, /health)
//!   2. Reply commands (QUARANTINE, RETRY, DISMISS — resolved via ConversationState)
//!   3. Conversational text (free text routed to the inference service)

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::Result;
use crate::inbound::conversation_state::{ContextResolution, ConversationState};
use crate::inbound::telegram_receiver::InboundMessage;
use crate::inbound::workflow_dispatch::{WorkflowDispatcher, WorkflowIntent};

/// Parsed command from operator text
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedCommand {
    /// Raw command verb (e.g., "status", "quarantine", "pipeline")
    pub command: String,
    /// Parameters (e.g., pipeline name, site ID)
    pub params: Vec<String>,
    /// Whether this was a slash command
    pub is_slash: bool,
    /// Whether this looks like a reply-command (single uppercase word)
    pub is_reply_command: bool,
}

/// Action to take after routing a command
#[derive(Debug)]
pub enum CommandAction {
    /// Execute locally within COMMS (e.g., /status, /health)
    Local {
        command: String,
        params: Vec<String>,
    },
    /// Route to the inference service (conversational text)
    Inference {
        prompt: String,
        pipeline: String,
        session_id: String,
    },
    /// Route reply to a component via callback stream
    ComponentReply {
        resolution: ContextResolution,
    },
    /// Pipeline switch command
    SetPipeline {
        pipeline: String,
    },
    /// Reset conversation state
    Reset,
    /// /start command — show welcome message with inline keyboard
    Start,
    /// Callback query from inline keyboard button press
    Callback {
        /// callback_data string (e.g., "pipeline_sysadmin", "settings", "about")
        data: String,
        /// Callback query ID (for answering)
        query_id: String,
        /// Message ID that had the keyboard (for editing)
        message_id: Option<i64>,
    },
    /// Show conversation history
    History,
    /// Staged inference chain (draft → review → guard)
    ChainInference {
        prompt: String,
        session_id: String,
    },
    /// Dispatch a workflow to the workflow engine
    WorkflowDispatch {
        intent: WorkflowIntent,
    },
    /// Unknown or empty — no action
    NoOp {
        reason: String,
    },
}

/// Routes inbound operator messages to appropriate handlers
pub struct CommandRouter {
    /// Known slash commands handled locally
    local_commands: Vec<&'static str>,
    /// Workflow dispatcher for recognizing workflow intents
    workflow_dispatcher: Option<WorkflowDispatcher>,
    /// Whether the staged inference chain is enabled
    pub chain_enabled: bool,
}

impl CommandRouter {
    pub fn new() -> Self {
        Self {
            local_commands: vec![
                "status", "health", "help", "stats", "sites", "visitors",
            ],
            workflow_dispatcher: None,
            chain_enabled: false,
        }
    }

    /// Enable workflow dispatch with the given dispatcher
    pub fn with_workflow_dispatcher(mut self, dispatcher: WorkflowDispatcher) -> Self {
        self.workflow_dispatcher = Some(dispatcher);
        self
    }

    /// Enable the staged inference chain
    pub fn with_inference_chain(mut self, enabled: bool) -> Self {
        self.chain_enabled = enabled;
        self
    }

    /// Route an inbound message to the appropriate action.
    ///
    /// Routing priority:
    ///   0. Callback queries → Callback action (button presses)
    ///   1. Slash commands → Local or SetPipeline or Reset or Start
    ///   2. Reply commands (uppercase) → resolve against active context
    ///   3. Free text → inference service
    pub async fn route_command(
        &self,
        msg: &InboundMessage,
        conv_state: &mut ConversationState,
        site_id: &str,
        pipeline: &str,
        session_id: &str,
    ) -> Result<CommandAction> {
        // 0. Callback queries (inline keyboard button presses)
        if msg.is_callback {
            return Ok(CommandAction::Callback {
                data: msg.text.clone(), // callback_data is stored in text field
                query_id: msg.callback_query_id.clone(),
                message_id: msg.callback_message_id,
            });
        }

        let parsed = self.parse_text(&msg.text);

        // 1. Slash commands
        if parsed.is_slash {
            return Ok(self.handle_slash(&parsed, pipeline));
        }

        // 2. Reply commands — check if text matches an active context
        if parsed.is_reply_command {
            if let Some(resolution) = conv_state
                .resolve_reply(&msg.chat_id, site_id, &msg.text)
                .await?
            {
                return Ok(CommandAction::ComponentReply { resolution });
            }

            // 2b. Check if it's a workflow intent (UPDATE, BACKUP, LOCKDOWN, etc.)
            if let Some(ref dispatcher) = self.workflow_dispatcher {
                if let Some(intent) = dispatcher.recognize_intent(&msg.text) {
                    return Ok(CommandAction::WorkflowDispatch { intent });
                }
            }

            // Not a valid reply or workflow — fall through to inference
        }

        // 3. Conversational text → inference service
        // Use the staged chain if enabled, otherwise direct single-pipeline
        if self.chain_enabled {
            Ok(CommandAction::ChainInference {
                prompt: msg.text.clone(),
                session_id: session_id.to_string(),
            })
        } else {
            Ok(CommandAction::Inference {
                prompt: msg.text.clone(),
                pipeline: pipeline.to_string(),
                session_id: session_id.to_string(),
            })
        }
    }

    /// Parse operator text into a structured command
    pub fn parse_text(&self, text: &str) -> ParsedCommand {
        let trimmed = text.trim();

        // Slash command: /status, /pipeline sysadmin, /reset
        if trimmed.starts_with('/') {
            let without_slash = &trimmed[1..];
            let mut parts = without_slash.splitn(2, char::is_whitespace);
            let command = parts.next().unwrap_or("").to_lowercase();
            let params: Vec<String> = parts
                .next()
                .map(|rest| {
                    rest.split_whitespace()
                        .map(|s| s.to_string())
                        .collect()
                })
                .unwrap_or_default();

            return ParsedCommand {
                command,
                params,
                is_slash: true,
                is_reply_command: false,
            };
        }

        // Reply command: single uppercase word or short uppercase phrase
        // e.g., "QUARANTINE", "RETRY", "DISMISS", "UPDATE your_site"
        let is_reply = Self::looks_like_reply_command(trimmed);

        if is_reply {
            let mut parts = trimmed.splitn(2, char::is_whitespace);
            let command = parts.next().unwrap_or("").to_uppercase();
            let params: Vec<String> = parts
                .next()
                .map(|rest| {
                    rest.split_whitespace()
                        .map(|s| s.to_string())
                        .collect()
                })
                .unwrap_or_default();

            return ParsedCommand {
                command,
                params,
                is_slash: false,
                is_reply_command: true,
            };
        }

        // Free text / conversational
        ParsedCommand {
            command: String::new(),
            params: vec![],
            is_slash: false,
            is_reply_command: false,
        }
    }

    /// Check if text looks like a reply command (uppercase action word)
    fn looks_like_reply_command(text: &str) -> bool {
        let first_word = text.split_whitespace().next().unwrap_or("");

        // Must be at least 3 chars and all uppercase letters
        first_word.len() >= 3
            && first_word.chars().all(|c| c.is_ascii_uppercase() || c == '_')
            && text.split_whitespace().count() <= 4 // reply commands are short
    }

    /// Handle a slash command
    fn handle_slash(&self, parsed: &ParsedCommand, current_pipeline: &str) -> CommandAction {
        match parsed.command.as_str() {
            // Start — welcome message with inline keyboard
            "start" => CommandAction::Start,

            // History — show recent conversation turns
            "history" => CommandAction::History,

            // Pipeline management
            "pipeline" | "p" => {
                if let Some(name) = parsed.params.first() {
                    CommandAction::SetPipeline {
                        pipeline: name.clone(),
                    }
                } else {
                    // No arg — show current pipeline (handled as local info)
                    CommandAction::Local {
                        command: "pipeline_info".to_string(),
                        params: vec![current_pipeline.to_string()],
                    }
                }
            }

            // Reset conversation
            "reset" | "clear" | "new" => CommandAction::Reset,

            // Local commands (COMMS answers directly)
            cmd if self.local_commands.contains(&cmd) => CommandAction::Local {
                command: cmd.to_string(),
                params: parsed.params.clone(),
            },

            // Unknown slash command — treat as inference with the slash stripped
            _ => {
                debug!(
                    command = %parsed.command,
                    "Unknown slash command, routing to inference"
                );
                CommandAction::Inference {
                    prompt: format!("/{} {}", parsed.command, parsed.params.join(" ")),
                    pipeline: current_pipeline.to_string(),
                    session_id: String::new(),
                }
            }
        }
    }
}
