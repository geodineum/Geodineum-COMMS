//! Message history endpoints

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::api::server::AppState;

#[derive(Deserialize)]
pub struct MessageQuery {
    pub site_id: Option<String>,
    pub status: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Serialize)]
pub struct MessageListResponse {
    pub messages: Vec<MessageSummary>,
    pub total: usize,
}

#[derive(Serialize)]
pub struct MessageSummary {
    pub id: String,
    pub site_id: String,
    pub message_type: String,
    pub status: String,
    pub timestamp: String,
    pub subject: Option<String>,
}

#[derive(Serialize)]
pub struct MessageDetailResponse {
    pub id: String,
    pub found: bool,
    pub message: Option<serde_json::Value>,
    pub dispatch_history: Vec<DispatchEvent>,
}

#[derive(Serialize)]
pub struct DispatchEvent {
    pub timestamp: String,
    pub channel: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct RetryResponse {
    pub success: bool,
    pub message: String,
}

pub async fn list_messages(
    State(_state): State<Arc<AppState>>,
    Query(query): Query<MessageQuery>,
) -> Result<Json<MessageListResponse>, StatusCode> {
    // TODO: Implement message listing from ValKey
    // For now, return empty list
    Ok(Json(MessageListResponse {
        messages: vec![],
        total: 0,
    }))
}

pub async fn get_message(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<MessageDetailResponse>, StatusCode> {
    // TODO: Implement message detail retrieval
    Ok(Json(MessageDetailResponse {
        id,
        found: false,
        message: None,
        dispatch_history: vec![],
    }))
}

pub async fn retry_message(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<RetryResponse>, StatusCode> {
    // TODO: Implement retry logic
    Ok(Json(RetryResponse {
        success: false,
        message: format!("Retry not implemented for message {}", id),
    }))
}
