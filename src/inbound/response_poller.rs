//! Inference response poller — polls the inference unified stream for responses
//! and delivers them back to the operator via Telegram.
//!
//! Mirrors the upstream request-response pattern:
//! XREVRANGE polling with backoff, typing indicator, message edit on completion.

use redis::aio::MultiplexedConnection;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{error, info, warn};

use crate::error::Result;

/// Parameters for a single inference response poll cycle
#[derive(Clone)]
pub struct PollRequest {
    /// Telegram bot token (for sending responses)
    pub bot_token: String,
    /// Telegram chat_id to deliver response to
    pub chat_id: String,
    /// Message ID of the "Processing..." placeholder (to edit)
    pub processing_msg_id: i64,
    /// ValKey stream to poll for response
    pub unified_stream: String,
    /// Request ID to match in the stream
    pub request_id: String,
    /// Pipeline name (for footer)
    pub pipeline: String,
    /// Maximum time to wait for response (seconds)
    pub timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
struct TelegramApiResponse {
    ok: bool,
    result: Option<TelegramApiMessage>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramApiMessage {
    message_id: i64,
}

/// Send a "Processing..." message and return its message_id
pub async fn send_processing_indicator(
    bot_token: &str,
    chat_id: &str,
    pipeline: &str,
) -> Result<i64> {
    let client = Client::new();
    let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token);

    let payload = serde_json::json!({
        "chat_id": chat_id,
        "text": format!("Processing via {}...", pipeline),
        "parse_mode": "HTML",
    });

    let response = client
        .post(&url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| crate::error::CommsError::Telegram(format!("sendMessage failed: {}", e)))?;

    let body: TelegramApiResponse = response
        .json()
        .await
        .map_err(|e| crate::error::CommsError::Telegram(format!("parse failed: {}", e)))?;

    if body.ok {
        Ok(body.result.map(|m| m.message_id).unwrap_or(0))
    } else {
        Err(crate::error::CommsError::Telegram(
            body.description.unwrap_or_else(|| "sendMessage failed".into()),
        ))
    }
}

/// Spawn a background task that:
/// 1. Sends typing indicators every 5s
/// 2. Polls XREVRANGE for the response
/// 3. Edits the processing message with the result
///
/// This runs in a spawned tokio task so it doesn't block the main loop.
pub fn spawn_response_poller(mut conn: MultiplexedConnection, req: PollRequest) {
    tokio::spawn(async move {
        let client = Client::new();
        let api_base = format!("https://api.telegram.org/bot{}", req.bot_token);

        let deadline = tokio::time::Instant::now()
            + Duration::from_secs(req.timeout_secs);
        let mut poll_interval = Duration::from_millis(1000);
        let mut typing_interval = tokio::time::interval(Duration::from_secs(5));

        // Send initial typing indicator
        send_typing(&client, &api_base, &req.chat_id).await;

        info!(
            request_id = %req.request_id,
            chat_id = %req.chat_id,
            pipeline = %req.pipeline,
            "Response poller started"
        );

        loop {
            tokio::select! {
                // Typing indicator tick
                _ = typing_interval.tick() => {
                    send_typing(&client, &api_base, &req.chat_id).await;
                }
                // Poll for response
                _ = tokio::time::sleep(poll_interval) => {
                    match poll_response(&mut conn, &req.unified_stream, &req.request_id).await {
                        Ok(Some(response)) => {
                            // Got response — edit the processing message
                            let text = format_response(&response, &req.pipeline);
                            let text_len = text.len();
                            info!(
                                request_id = %req.request_id,
                                chat_id = %req.chat_id,
                                response_len = text_len,
                                "Response received, delivering to Telegram"
                            );
                            let delivered = edit_message(
                                &client, &api_base, &req.chat_id, req.processing_msg_id, &text,
                            ).await;
                            if delivered {
                                info!(
                                    request_id = %req.request_id,
                                    chat_id = %req.chat_id,
                                    "Response delivered successfully"
                                );
                            } else {
                                warn!(
                                    request_id = %req.request_id,
                                    chat_id = %req.chat_id,
                                    "Response generated but Telegram delivery FAILED (both edit and send fallback)"
                                );
                            }
                            return;
                        }
                        Ok(None) => {
                            // No response yet — back off slightly (matches GeodineBridge pattern)
                            if poll_interval < Duration::from_millis(3000) {
                                poll_interval += Duration::from_millis(500);
                            }
                        }
                        Err(e) => {
                            error!(error = %e, request_id = %req.request_id, "Poll error");
                        }
                    }

                    // Check timeout
                    if tokio::time::Instant::now() >= deadline {
                        warn!(
                            request_id = %req.request_id,
                            timeout_secs = req.timeout_secs,
                            "Inference response timeout"
                        );
                        let _ = edit_message(
                            &client,
                            &api_base,
                            &req.chat_id,
                            req.processing_msg_id,
                            "Inference timed out. The pipeline may be warming up — try again in a moment.",
                        ).await;
                        return;
                    }
                }
            }
        }
    });
}

/// Poll XREVRANGE for a response entry matching request_id.
///
/// Parses the raw redis::Value manually because the redis crate doesn't
/// have a built-in FromRedisValue for the nested XREVRANGE response shape.
async fn poll_response(
    conn: &mut MultiplexedConnection,
    stream: &str,
    request_id: &str,
) -> std::result::Result<Option<HashMap<String, String>>, redis::RedisError> {
    let raw: redis::Value = redis::cmd("XREVRANGE")
        .arg(stream)
        .arg("+")
        .arg("-")
        .arg("COUNT")
        .arg(20)
        .query_async(conn)
        .await?;

    let entries = parse_xrange_response(&raw);

    for (_entry_id, fields) in entries {
        if fields.get("id").map(|s| s.as_str()) == Some(request_id)
            && fields.contains_key("status")
        {
            return Ok(Some(fields));
        }
    }

    Ok(None)
}

/// Format a inference response for Telegram delivery
fn format_response(fields: &HashMap<String, String>, pipeline: &str) -> String {
    let status = fields.get("status").map(|s| s.as_str()).unwrap_or("unknown");

    if status != "ok" {
        let error = fields
            .get("error")
            .cloned()
            .unwrap_or_else(|| "Unknown error".to_string());
        // an earlier hardening pass: `error` is AI/backend-generated; escape before HTML parse_mode.
        return format!(
            "Error from {}: {}",
            crate::inbound::html_escape_telegram(pipeline),
            crate::inbound::html_escape_telegram(&error)
        );
    }

    // Parse result JSON (contains text, metrics, session_id)
    let result_raw = fields.get("result").cloned().unwrap_or_default();
    let result: serde_json::Value =
        serde_json::from_str(&result_raw).unwrap_or(serde_json::Value::Null);

    let text = result
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or(&result_raw);

    // Build metrics footer (matches the standard telegram footer format)
    let metrics = result.get("metrics").cloned().unwrap_or_default();
    let footer = format_metrics_footer(pipeline, &metrics);

    // an earlier hardening pass: `text` is AI-generated; escape before embedding into the
    // HTML-parsed footer wrapper. `footer` is generated by us from typed
    // numeric fields (pipeline name + tokens/ms metrics) so it's safe —
    // but pipeline name itself comes from user input, so escape it too.
    let safe_text = crate::inbound::html_escape_telegram(text);
    if footer.is_empty() {
        safe_text
    } else {
        format!("{}\n\n<i>{}</i>", safe_text, crate::inbound::html_escape_telegram(&footer))
    }
}

/// Format metrics footer (ported from telegram-inference-bridge's format_metrics_footer)
fn format_metrics_footer(pipeline: &str, metrics: &serde_json::Value) -> String {
    let mut parts = vec![pipeline.to_string()];

    if let Some(tokens) = metrics.get("tokens_generated").and_then(|v| v.as_u64()) {
        if tokens > 0 {
            parts.push(format!("{} tokens", tokens));
        }
    }

    if let Some(time_ms) = metrics.get("time_ms").and_then(|v| v.as_f64()) {
        if time_ms > 0.0 {
            parts.push(format!("{:.1}s", time_ms / 1000.0));
        }
    }

    if let Some(tok_s) = metrics.get("tokens_per_sec").and_then(|v| v.as_f64()) {
        if tok_s > 0.0 {
            parts.push(format!("{:.1} tok/s", tok_s));
        }
    }

    let cache = if metrics
        .get("system_cache_hit")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        "cached"
    } else {
        "cold"
    };
    parts.push(cache.to_string());

    parts.join(" | ")
}

/// Send typing indicator
async fn send_typing(client: &Client, api_base: &str, chat_id: &str) {
    let url = format!("{}/sendChatAction", api_base);
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "action": "typing",
    });
    let _ = client.post(&url).json(&payload).send().await;
}

/// Edit an existing Telegram message. Returns true if delivery succeeded
/// (either edit or fallback send). The HTML parse_mode is used first; on
/// Telegram rejection (e.g., invalid markup in AI output), retries with
/// plain text so the user still receives the reply.
async fn edit_message(
    client: &Client,
    api_base: &str,
    chat_id: &str,
    message_id: i64,
    text: &str,
) -> bool {
    let url = format!("{}/editMessageText", api_base);
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "text": text,
        "parse_mode": "HTML",
    });

    // Attempt 1: edit with HTML
    if let Ok(resp) = client.post(&url).json(&payload).send().await {
        if resp.status().is_success() {
            return true;
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        warn!(
            chat_id = %chat_id,
            status = %status,
            body = %body,
            "editMessageText HTML failed, retrying plain"
        );
    }

    // Attempt 2: edit with no parse_mode (plain text) — AI output may contain bad HTML
    let plain_payload = serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "text": text,
    });
    if let Ok(resp) = client.post(&url).json(&plain_payload).send().await {
        if resp.status().is_success() {
            return true;
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        warn!(
            chat_id = %chat_id,
            status = %status,
            body = %body,
            "editMessageText plain also failed, falling back to sendMessage"
        );
    }

    // Attempt 3: send a new message (plain) — last resort so user at least gets the reply
    let send_url = format!("{}/sendMessage", api_base);
    let send_payload = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
    });
    match client.post(&send_url).json(&send_payload).send().await {
        Ok(resp) if resp.status().is_success() => true,
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!(
                chat_id = %chat_id,
                status = %status,
                body = %body,
                "sendMessage fallback also failed — reply LOST"
            );
            false
        }
        Err(e) => {
            warn!(
                chat_id = %chat_id,
                error = %crate::error::scrub_reqwest_url(&e),
                "sendMessage fallback errored — reply LOST"
            );
            false
        }
    }
}

/// Parse a raw XREVRANGE / XRANGE response into [(entry_id, fields)].
///
/// Expected shape:
///   Bulk([
///     Bulk([ entry_id, Bulk([field, value, field, value, ...]) ]),
///     ...
///   ])
fn parse_xrange_response(value: &redis::Value) -> Vec<(String, HashMap<String, String>)> {
    let entries = match value {
        redis::Value::Array(e) => e,
        _ => return Vec::new(),
    };

    let mut out = Vec::new();
    for entry in entries {
        let entry_parts = match entry {
            redis::Value::Array(p) => p,
            _ => continue,
        };
        if entry_parts.len() < 2 {
            continue;
        }

        let entry_id: String = match redis::from_redis_value(&entry_parts[0]) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let field_values: Vec<String> = match redis::from_redis_value(&entry_parts[1]) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let mut fields = HashMap::new();
        let mut iter = field_values.into_iter();
        while let (Some(k), Some(v)) = (iter.next(), iter.next()) {
            fields.insert(k, v);
        }

        out.push((entry_id, fields));
    }

    out
}
