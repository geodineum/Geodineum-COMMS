//! Staged inference chain — draft → review → guard
//!
//! Three-stage chain using the same ValKey XADD/XREVRANGE protocol:
//!
//!   DRAFT  (small model) — fast first-pass response
//!   REVIEW (larger model) — substantive response informed by the draft
//!                           (prompt = original + "[Prior draft: …]")
//!   GUARD  (guard model)  — safety gate on the review response:
//!                           safe → deliver; unsafe → "[Response filtered]"
//!
//! No new infrastructure — all three stages use the inference unified
//! stream. Stage pipeline names come from env (COMMS_CHAIN_*_PIPELINE).

use redis::aio::MultiplexedConnection;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Result of a staged inference chain
#[derive(Debug)]
pub struct InferenceChainResult {
    /// The final response text to deliver to the operator
    pub text: String,
    /// Whether the guard stage filtered the response
    pub filtered: bool,
    /// Filter reason (if filtered)
    pub filter_reason: Option<String>,
    /// Draft-stage response (for diagnostics)
    pub draft_response: String,
    /// Review-stage response (the substantive answer)
    pub review_response: String,
    /// Aggregate metrics from all three stages
    pub metrics: serde_json::Value,
}

/// Pipeline configuration for each stage
#[derive(Debug, Clone)]
pub struct InferenceChainConfig {
    /// Draft stage: fast first pass (small model)
    pub draft_pipeline: String,
    /// Review stage: substantive reasoning (larger model)
    pub review_pipeline: String,
    /// Guard stage: safety/policy gate (guard model)
    pub guard_pipeline: String,
    /// Timeout per stage in seconds
    pub stage_timeout_secs: u64,
}

impl Default for InferenceChainConfig {
    fn default() -> Self {
        Self {
            draft_pipeline: "draft".to_string(),
            review_pipeline: "review".to_string(),
            guard_pipeline: "guard".to_string(),
            stage_timeout_secs: 300, // 5 min per stage
        }
    }
}

impl InferenceChainConfig {
    /// Build from env, falling back to defaults:
    ///   COMMS_CHAIN_DRAFT_PIPELINE / COMMS_CHAIN_REVIEW_PIPELINE /
    ///   COMMS_CHAIN_GUARD_PIPELINE / COMMS_CHAIN_STAGE_TIMEOUT_SECS
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("COMMS_CHAIN_DRAFT_PIPELINE") {
            if !v.is_empty() { cfg.draft_pipeline = v; }
        }
        if let Ok(v) = std::env::var("COMMS_CHAIN_REVIEW_PIPELINE") {
            if !v.is_empty() { cfg.review_pipeline = v; }
        }
        if let Ok(v) = std::env::var("COMMS_CHAIN_GUARD_PIPELINE") {
            if !v.is_empty() { cfg.guard_pipeline = v; }
        }
        if let Ok(v) = std::env::var("COMMS_CHAIN_STAGE_TIMEOUT_SECS") {
            if let Ok(n) = v.parse() { cfg.stage_timeout_secs = n; }
        }
        cfg
    }
}

/// Run the staged inference chain.
///
/// This is a sequential three-step pipeline:
///   1. draft: fast first pass
///   2. review: substantive answer with draft context
///   3. guard: safety gate on the review output
pub async fn run_inference_chain(
    conn: &mut MultiplexedConnection,
    unified_stream: &str,
    prompt: &str,
    operator_id: &str,
    session_id: &str,
    config: &InferenceChainConfig,
) -> Result<InferenceChainResult, String> {
    let base_request_id = format!(
        "chain-{}-{}",
        operator_id,
        chrono::Utc::now().timestamp_millis()
    );

    // ── Stage 1: draft (fast first pass) ────────────────────────────
    let draft_request_id = format!("{}-draft", base_request_id);

    info!(pipeline = %config.draft_pipeline, "Inference chain: draft stage");

    xadd_inference(
        conn,
        unified_stream,
        &draft_request_id,
        &config.draft_pipeline,
        prompt,
        operator_id,
        session_id,
    )
    .await?;

    let draft_response = poll_response(
        conn,
        unified_stream,
        &draft_request_id,
        config.stage_timeout_secs,
    )
    .await?;

    let draft_text = extract_response_text(&draft_response);
    debug!(draft_response = %draft_text, "Inference chain: draft complete");

    // ── Stage 2: review (substantive, informed by the draft) ─────────
    let review_request_id = format!("{}-review", base_request_id);
    let review_prompt = format!(
        "{}\n\n[Prior draft: {}]",
        prompt, draft_text
    );

    info!(pipeline = %config.review_pipeline, "Inference chain: review stage");

    xadd_inference(
        conn,
        unified_stream,
        &review_request_id,
        &config.review_pipeline,
        &review_prompt,
        operator_id,
        session_id,
    )
    .await?;

    let review_response = poll_response(
        conn,
        unified_stream,
        &review_request_id,
        config.stage_timeout_secs,
    )
    .await?;

    let review_text = extract_response_text(&review_response);
    let review_metrics = extract_metrics(&review_response);
    debug!(review_response_len = review_text.len(), "Inference chain: review complete");

    // ── Stage 3: guard (safety gate) ─────────────────────────────────
    let guard_request_id = format!("{}-guard", base_request_id);

    info!(pipeline = %config.guard_pipeline, "Inference chain: guard stage");

    // The guard pipeline has its own system prompt that defines its role
    // as the safety filter. We pass the structured input it expects:
    // [OPERATOR QUESTION] + [AI RESPONSE TO EVALUATE]
    let guard_prompt = format!(
        "[OPERATOR QUESTION]\n{}\n\n[AI RESPONSE TO EVALUATE]\n{}",
        prompt, review_text
    );

    xadd_inference(
        conn,
        unified_stream,
        &guard_request_id,
        &config.guard_pipeline,
        &guard_prompt,
        operator_id,
        "", // no session for safety check
    )
    .await?;

    let guard_response = poll_response(
        conn,
        unified_stream,
        &guard_request_id,
        config.stage_timeout_secs,
    )
    .await?;

    let guard_text = extract_response_text(&guard_response);

    // Parse the guard verdict — guard models return "safe" or "unsafe\n<category>"
    let is_safe = guard_text.trim().to_lowercase().starts_with("safe");

    if is_safe {
        info!("Inference chain: guard passed — delivering review response");
        Ok(InferenceChainResult {
            text: review_text.clone(),
            filtered: false,
            filter_reason: None,
            draft_response: draft_text,
            review_response: review_text,
            metrics: review_metrics,
        })
    } else {
        let reason = guard_text
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join(", ");
        let reason = if reason.is_empty() {
            "Policy violation detected".to_string()
        } else {
            reason
        };

        warn!(reason = %reason, "Inference chain: guard filtered response");

        Ok(InferenceChainResult {
            text: format!(
                "[Response filtered by safety policy]\nReason: {}",
                reason
            ),
            filtered: true,
            filter_reason: Some(reason),
            draft_response: draft_text,
            review_response: review_text,
            metrics: review_metrics,
        })
    }
}

/// XADD an inference request to the unified stream
async fn xadd_inference(
    conn: &mut MultiplexedConnection,
    stream: &str,
    request_id: &str,
    pipeline: &str,
    prompt: &str,
    consumer: &str,
    session_id: &str,
) -> Result<(), String> {
    let params = serde_json::json!({
        "prompt": prompt,
        "pipeline": pipeline,
        "consumer": format!("comms:{}", consumer),
        "session_id": session_id,
    });

    let result: redis::RedisResult<String> = redis::cmd("XADD")
        .arg(stream)
        .arg("*")
        .arg(&[
            ("id", request_id),
            ("cmd", "direct"),
            ("params", &params.to_string()),
            ("_gh", "inference"),
        ])
        .query_async(conn)
        .await;

    result
        .map(|_| ())
        .map_err(|e| format!("XADD failed for {}: {}", request_id, e))
}

/// Poll XREVRANGE for a response matching request_id
async fn poll_response(
    conn: &mut MultiplexedConnection,
    stream: &str,
    request_id: &str,
    timeout_secs: u64,
) -> Result<HashMap<String, String>, String> {
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut interval = Duration::from_millis(1000);

    loop {
        let entries: Vec<(String, HashMap<String, String>)> = redis::cmd("XREVRANGE")
            .arg(stream)
            .arg("+")
            .arg("-")
            .arg("COUNT")
            .arg(20)
            .query_async(conn)
            .await
            .map_err(|e| format!("XREVRANGE failed: {}", e))?;

        for (_entry_id, fields) in entries {
            if fields.get("id").map(|s| s.as_str()) == Some(request_id)
                && fields.contains_key("status")
            {
                let status = fields.get("status").map(|s| s.as_str()).unwrap_or("");
                if status == "ok" || status == "error" {
                    return Ok(fields);
                }
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "Timeout waiting for response {} after {}s",
                request_id, timeout_secs
            ));
        }

        tokio::time::sleep(interval).await;
        if interval < Duration::from_millis(3000) {
            interval += Duration::from_millis(500);
        }
    }
}

/// Extract response text from a inference response
fn extract_response_text(fields: &HashMap<String, String>) -> String {
    let status = fields.get("status").map(|s| s.as_str()).unwrap_or("");

    if status == "error" {
        return fields
            .get("error")
            .cloned()
            .unwrap_or_else(|| "Unknown error".to_string());
    }

    let result_raw = fields.get("result").cloned().unwrap_or_default();
    let result: serde_json::Value =
        serde_json::from_str(&result_raw).unwrap_or(serde_json::Value::Null);

    result
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or(&result_raw)
        .to_string()
}

/// Extract metrics from a inference response
fn extract_metrics(fields: &HashMap<String, String>) -> serde_json::Value {
    let result_raw = fields.get("result").cloned().unwrap_or_default();
    let result: serde_json::Value =
        serde_json::from_str(&result_raw).unwrap_or(serde_json::Value::Null);

    result
        .get("metrics")
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}
