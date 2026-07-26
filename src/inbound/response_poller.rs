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
                            let (raw, is_error, metrics) = extract_response(&response);

                            // Chunk on the RAW (think-stripped) text first —
                            // paragraph/line boundaries don't split inline
                            // formatting — then render each chunk to HTML
                            // independently, so no tag ever spans a boundary.
                            let mut chunks: Vec<String> = if is_error {
                                let body = format!(
                                    "Error from {}: {}",
                                    crate::inbound::html_escape_telegram(&req.pipeline),
                                    crate::inbound::html_escape_telegram(&raw)
                                );
                                split_message(&body, CHUNK)
                            } else {
                                split_message(&strip_thinking(&raw), CHUNK)
                                    .iter()
                                    .map(|c| crate::inbound::markdown_to_telegram_html(c))
                                    .collect()
                            };
                            if chunks.is_empty() {
                                chunks.push(String::new());
                            }
                            if !is_error {
                                // Footer only on the last chunk so its <i> tag is never split.
                                let footer = format_metrics_footer(&req.pipeline, &metrics);
                                if !footer.is_empty() {
                                    let tail = format!(
                                        "\n\n<i>{}</i>",
                                        crate::inbound::html_escape_telegram(&footer)
                                    );
                                    let last = chunks.last_mut().unwrap();
                                    if last.chars().count() + tail.chars().count() <= TG_LIMIT {
                                        last.push_str(&tail);
                                    } else {
                                        chunks.push(format!(
                                            "<i>{}</i>",
                                            crate::inbound::html_escape_telegram(&footer)
                                        ));
                                    }
                                }
                            }

                            info!(
                                request_id = %req.request_id,
                                chat_id = %req.chat_id,
                                response_len = raw.chars().count(),
                                chunks = chunks.len(),
                                "Response received, delivering to Telegram"
                            );
                            let delivered = deliver_chunks(
                                &client, &api_base, &req.chat_id, req.processing_msg_id, &chunks,
                            )
                            .await;

                            // Durable fallback log — the FULL raw text (incl any
                            // <think> trace), so a reply is never lost even if
                            // Telegram delivery fails.
                            log_response(
                                &mut conn, &req.unified_stream, &req.request_id,
                                &req.chat_id, &req.pipeline, &raw, delivered,
                            )
                            .await;

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
                                    "Telegram delivery FAILED — full response preserved in {}:comms:responses",
                                    site_prefix(&req.unified_stream)
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
                        let _ = send_chunk(
                            &client,
                            &api_base,
                            &req.chat_id,
                            Some(req.processing_msg_id),
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

/// Extract the RAW model text (with any reasoning trace intact — the durable
/// log keeps the complete copy), an is_error flag, and the metrics JSON. All
/// front-end shaping (think-strip, HTML-escape, footer, chunking) happens in
/// the poller so the logged text is never lossy.
fn extract_response(fields: &HashMap<String, String>) -> (String, bool, serde_json::Value) {
    let status = fields.get("status").map(|s| s.as_str()).unwrap_or("unknown");
    if status != "ok" {
        let error = fields
            .get("error")
            .cloned()
            .unwrap_or_else(|| "Unknown error".to_string());
        return (error, true, serde_json::Value::Null);
    }

    let result_raw = fields.get("result").cloned().unwrap_or_default();
    let result: serde_json::Value =
        serde_json::from_str(&result_raw).unwrap_or(serde_json::Value::Null);
    let text = result
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or(&result_raw)
        .to_string();
    let metrics = result.get("metrics").cloned().unwrap_or(serde_json::Value::Null);
    (text, false, metrics)
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

/// Telegram hard cap per message (UTF-16 code units). We chunk on char count,
/// a safe under-approximation for BMP text, with headroom for the footer.
const TG_LIMIT: usize = 4096;
/// Split raw text at this size; markdown->HTML rendering then expands each
/// chunk with tags, so leave headroom under the 4096 cap.
const CHUNK: usize = 3500;

/// Derive the site prefix (e.g. "{geodine}") from a unified stream key like
/// "{geodine}:gnode:unified:production", for building the response-log key.
fn site_prefix(unified_stream: &str) -> &str {
    unified_stream.split(":gnode:").next().unwrap_or("{geodine}")
}

/// Strip <think>/<thinking> reasoning blocks from model output for front-end
/// delivery. The FULL text (with the trace) is still logged durably by
/// log_response, so nothing is lost — the trace is just not shown. Handles
/// multiple blocks and an unclosed block (a trace truncated at max_tokens).
fn strip_thinking(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let open = ["<think>", "<thinking>"]
            .iter()
            .filter_map(|t| rest.find(t).map(|i| (i, *t)))
            .min_by_key(|(i, _)| *i);
        match open {
            Some((start, tag)) => {
                out.push_str(&rest[..start]);
                let after = &rest[start + tag.len()..];
                let close = if tag == "<think>" { "</think>" } else { "</thinking>" };
                match after.find(close) {
                    Some(end) => rest = &after[end + close.len()..],
                    None => break, // unclosed trace — drop the remainder
                }
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out.trim().to_string()
}

/// Split text into <= max-char chunks on the softest available boundary
/// (paragraph, then line, then hard char split). Never drops content.
fn split_message(text: &str, max: usize) -> Vec<String> {
    if text.chars().count() <= max {
        return vec![text.to_string()];
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let clen = |s: &str| s.chars().count();
    let flush = |cur: &mut String, chunks: &mut Vec<String>| {
        if !cur.is_empty() {
            chunks.push(std::mem::take(cur));
        }
    };
    for para in text.split_inclusive("\n\n") {
        if clen(&cur) + clen(para) <= max {
            cur.push_str(para);
        } else if clen(para) <= max {
            flush(&mut cur, &mut chunks);
            cur.push_str(para);
        } else {
            flush(&mut cur, &mut chunks);
            for line in para.split_inclusive('\n') {
                if clen(&cur) + clen(line) <= max {
                    cur.push_str(line);
                } else if clen(line) <= max {
                    flush(&mut cur, &mut chunks);
                    cur.push_str(line);
                } else {
                    flush(&mut cur, &mut chunks);
                    for ch in line.chars() {
                        if clen(&cur) >= max {
                            flush(&mut cur, &mut chunks);
                        }
                        cur.push(ch);
                    }
                }
            }
        }
    }
    flush(&mut cur, &mut chunks);
    chunks
}

/// Durably record a generated response to a ValKey list so it is NEVER lost —
/// even when Telegram delivery fails. Logs the full (unstripped) text plus
/// delivery outcome as a JSON envelope; LTRIM caps the list. Best-effort: a
/// logging failure must never block delivery.
async fn log_response(
    conn: &mut MultiplexedConnection,
    unified_stream: &str,
    request_id: &str,
    chat_id: &str,
    pipeline: &str,
    full_text: &str,
    delivered: bool,
) {
    // Own-namespace only: COMMS is granted {site}:gnode:comms:* (its delivery
    // channel), NOT the bare {site}:* tree — writing there is denied NOPERM.
    let key = format!("{}:gnode:comms:responses", site_prefix(unified_stream));
    let envelope = serde_json::json!({
        "request_id": request_id,   // embeds a ms timestamp
        "chat_id": chat_id,
        "pipeline": pipeline,
        "delivered": delivered,
        "text": full_text,
    })
    .to_string();
    let res: redis::RedisResult<()> = redis::pipe()
        .cmd("LPUSH").arg(&key).arg(&envelope).ignore()
        .cmd("LTRIM").arg(&key).arg(0).arg(999).ignore()
        .query_async(conn)
        .await;
    if let Err(e) = res {
        warn!(request_id = %request_id, error = %e, "Failed to durably log response");
    }
}

/// Deliver an already-chunked, HTML-formatted reply. The first chunk edits the
/// "Processing…" placeholder; subsequent chunks are sent as new messages. Each
/// chunk independently falls back HTML -> plain so bad markup never loses text.
/// Returns true only if every chunk was delivered.
async fn deliver_chunks(
    client: &Client,
    api_base: &str,
    chat_id: &str,
    first_msg_id: i64,
    chunks: &[String],
) -> bool {
    let mut all = true;
    for (i, chunk) in chunks.iter().enumerate() {
        let ok = if i == 0 {
            send_chunk(client, api_base, chat_id, Some(first_msg_id), chunk).await
        } else {
            send_chunk(client, api_base, chat_id, None, chunk).await
        };
        all &= ok;
    }
    all
}

/// Send or edit one chunk: HTML parse_mode first, then plain text, then (when
/// editing) a fresh send as last resort. Returns true on any success.
async fn send_chunk(
    client: &Client,
    api_base: &str,
    chat_id: &str,
    message_id: Option<i64>,
    text: &str,
) -> bool {
    // Attempt 1 + 2: edit (if we have a message_id) with HTML then plain.
    if let Some(mid) = message_id {
        let edit_url = format!("{}/editMessageText", api_base);
        for (html, label) in [(true, "HTML"), (false, "plain")] {
            let mut payload = serde_json::json!({
                "chat_id": chat_id, "message_id": mid, "text": text,
            });
            if html {
                payload["parse_mode"] = serde_json::json!("HTML");
            }
            if let Ok(resp) = client.post(&edit_url).json(&payload).send().await {
                if resp.status().is_success() {
                    return true;
                }
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!(chat_id = %chat_id, status = %status, body = %body,
                    "editMessageText {label} failed");
            }
        }
    }

    // Send a new message: HTML then plain.
    let send_url = format!("{}/sendMessage", api_base);
    for (html, label) in [(true, "HTML"), (false, "plain")] {
        let mut payload = serde_json::json!({ "chat_id": chat_id, "text": text });
        if html {
            payload["parse_mode"] = serde_json::json!("HTML");
        }
        match client.post(&send_url).json(&payload).send().await {
            Ok(resp) if resp.status().is_success() => return true,
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!(chat_id = %chat_id, status = %status, body = %body,
                    "sendMessage {label} failed");
            }
            Err(e) => warn!(chat_id = %chat_id,
                error = %crate::error::scrub_reqwest_url(&e), "sendMessage {label} errored"),
        }
    }
    false
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
