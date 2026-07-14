//! Workflow dispatch — triggers workflows from operator commands
//!
//! Recognizes workflow intents from operator text and dispatches them to
//! the workflow engine's queue stream on ValKey. Delivers per-step
//! progress back to the operator.
//!
//! Workflow intents:
//!   "UPDATE your_site"   → rolling_update workflow
//!   "UPDATE ALL"          → rolling_update for all sites
//!   "BACKUP"              → backup workflow
//!   "LOCKDOWN"            → security lockdown
//!   "UNLOCK"              → release lockdown
//!   "RESTART geodine"     → service restart

use redis::aio::MultiplexedConnection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{info, warn};

// an earlier hardening pass whitelist: only these workflow_ids may reach the engine.
// Adding a new workflow REQUIRES editing this list AND `recognize_intent`
// in the same commit so the whitelist and the parser stay in lockstep.
const ALLOWED_WORKFLOW_IDS: &[&str] = &[
    "rolling_update",
    "backup",
    "lockdown",
    "unlock",
    "restart_service",
    "deploy",
];

// Param keys allowed in any workflow's params map. Free-form values
// would let an attacker smuggle structured data through to the engine;
// the parser only ever emits these three keys.
const ALLOWED_PARAM_KEYS: &[&str] = &["scope", "target", "service"];

/// Param values must be ASCII letters / digits / `_` / `-` (and the
/// special token "ALL"). This rejects whitespace, shell metacharacters,
/// path traversal, and unicode lookalikes — workflow-engine consumers
/// downstream don't need anything else from these slots.
fn is_safe_param_value(value: &str) -> bool {
    if value.is_empty() || value.len() > 64 {
        return false;
    }
    value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A recognized workflow intent from operator text
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowIntent {
    /// Workflow identifier (e.g., "rolling_update", "backup", "lockdown")
    pub workflow_id: String,
    /// Human-readable description
    pub description: String,
    /// Parameters for the workflow
    pub params: HashMap<String, String>,
}

/// Known workflow triggers and their mappings
pub struct WorkflowDispatcher {
    /// ValKey stream for the workflow-engine queue
    workflow_stream: String,
}

impl WorkflowDispatcher {
    pub fn new(site_id: &str, environment: &str) -> Self {
        Self {
            workflow_stream: format!(
                "{{{}}}:gnode:comms:workflows:{}",
                site_id, environment
            ),
        }
    }

    /// Try to recognize a workflow intent from operator text.
    /// Returns None if the text doesn't match any known workflow pattern.
    pub fn recognize_intent(&self, text: &str) -> Option<WorkflowIntent> {
        let upper = text.trim().to_uppercase();
        let parts: Vec<&str> = upper.split_whitespace().collect();

        if parts.is_empty() {
            return None;
        }

        match parts[0] {
            "UPDATE" => {
                let scope = parts.get(1).map(|s| s.to_string()).unwrap_or_else(|| "ALL".into());
                Some(WorkflowIntent {
                    workflow_id: "rolling_update".into(),
                    description: format!("Rolling update (scope: {})", scope),
                    params: [("scope".into(), scope)].into_iter().collect(),
                })
            }
            "BACKUP" => {
                let target = parts.get(1).map(|s| s.to_string()).unwrap_or_else(|| "ALL".into());
                Some(WorkflowIntent {
                    workflow_id: "backup".into(),
                    description: format!("Backup (target: {})", target),
                    params: [("target".into(), target)].into_iter().collect(),
                })
            }
            "LOCKDOWN" => {
                let target = parts.get(1).map(|s| s.to_string()).unwrap_or_else(|| "ALL".into());
                Some(WorkflowIntent {
                    workflow_id: "lockdown".into(),
                    description: format!("Security lockdown (target: {})", target),
                    params: [("target".into(), target)].into_iter().collect(),
                })
            }
            "UNLOCK" => {
                let target = parts.get(1).map(|s| s.to_string()).unwrap_or_else(|| "ALL".into());
                Some(WorkflowIntent {
                    workflow_id: "unlock".into(),
                    description: format!("Release lockdown (target: {})", target),
                    params: [("target".into(), target)].into_iter().collect(),
                })
            }
            "RESTART" => {
                let service = parts.get(1).map(|s| s.to_string())?;
                Some(WorkflowIntent {
                    workflow_id: "restart_service".into(),
                    description: format!("Restart service: {}", service),
                    params: [("service".into(), service)].into_iter().collect(),
                })
            }
            "DEPLOY" => {
                let target = parts.get(1).map(|s| s.to_string()).unwrap_or_else(|| "ALL".into());
                Some(WorkflowIntent {
                    workflow_id: "deploy".into(),
                    description: format!("Deploy (target: {})", target),
                    params: [("target".into(), target)].into_iter().collect(),
                })
            }
            _ => None,
        }
    }

    /// Dispatch a workflow intent to the workflow engine via ValKey stream.
    /// Returns a workflow execution ID for tracking.
    pub async fn dispatch(
        &self,
        conn: &mut MultiplexedConnection,
        intent: &WorkflowIntent,
        operator_id: &str,
        operator_name: &str,
    ) -> Result<String, String> {
        // an earlier hardening pass: whitelist gate. Even though `recognize_intent` only
        // emits known workflow_ids in normal use, dispatch() is reachable
        // from anywhere in the codebase and an attacker who slips into
        // admin_ids would otherwise have a workflow-engine invocation
        // primitive. Defense-in-depth: validate at the dispatch boundary.
        if !ALLOWED_WORKFLOW_IDS.contains(&intent.workflow_id.as_str()) {
            return Err(format!(
                "rejected: workflow_id '{}' not in dispatch whitelist",
                intent.workflow_id
            ));
        }
        for (key, value) in intent.params.iter() {
            if !ALLOWED_PARAM_KEYS.contains(&key.as_str()) {
                return Err(format!(
                    "rejected: workflow param key '{}' not in whitelist",
                    key
                ));
            }
            if !is_safe_param_value(value) {
                return Err(format!(
                    "rejected: workflow param value for '{}' contains disallowed characters",
                    key
                ));
            }
        }

        // Honest-failure gate: a dispatch is only meaningful if a
        // workflow engine is actually consuming the stream. Without this
        // check the XADD succeeds, the operator gets a confident
        // "dispatched" confirmation, and the entry rots unread — a false
        // affirmative. XINFO GROUPS errors when the stream doesn't
        // exist; an empty list means nothing ever registered to consume.
        let groups: redis::RedisResult<Vec<redis::Value>> = redis::cmd("XINFO")
            .arg("GROUPS")
            .arg(&self.workflow_stream)
            .query_async(conn)
            .await;
        let has_consumer = matches!(&groups, Ok(g) if !g.is_empty());
        if !has_consumer {
            info!(
                workflow_id = %intent.workflow_id,
                operator = %operator_name,
                "Workflow dispatch refused — no engine consumes the workflow stream"
            );
            return Err(
                "no workflow engine is connected (nothing consumes the workflow stream) — dispatch refused".to_string()
            );
        }

        let execution_id = format!(
            "wf-{}-{}",
            intent.workflow_id,
            chrono::Utc::now().timestamp_millis()
        );

        let params_json = serde_json::to_string(&intent.params)
            .map_err(|e| format!("Failed to serialize params: {}", e))?;

        let result: redis::RedisResult<String> = redis::cmd("XADD")
            .arg(&self.workflow_stream)
            .arg("*")
            .arg(&[
                ("execution_id", execution_id.as_str()),
                ("workflow_id", intent.workflow_id.as_str()),
                ("description", intent.description.as_str()),
                ("params", &params_json),
                ("operator_id", operator_id),
                ("operator_name", operator_name),
                ("status", "pending"),
                ("ts", &chrono::Utc::now().to_rfc3339()),
            ])
            .query_async(conn)
            .await;

        match result {
            Ok(_) => {
                info!(
                    workflow_id = %intent.workflow_id,
                    execution_id = %execution_id,
                    operator = %operator_name,
                    "Dispatched workflow to the workflow-engine stream"
                );
                Ok(execution_id)
            }
            Err(e) => {
                warn!(error = %e, "Failed to dispatch workflow");
                Err(format!("Workflow dispatch failed: {}", e))
            }
        }
    }

    /// Format a confirmation message for the operator after dispatching
    pub fn format_dispatch_confirmation(
        intent: &WorkflowIntent,
        execution_id: &str,
    ) -> String {
        format!(
            "Workflow dispatched: <b>{}</b>\n\
             Execution ID: <code>{}</code>\n\n\
             The workflow engine will process this and report progress.",
            intent.description, execution_id
        )
    }
}
