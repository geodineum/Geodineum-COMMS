//! Statistics endpoints

use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;
use std::sync::Arc;

use crate::api::server::AppState;

#[derive(Serialize)]
pub struct StatsResponse {
    pub total_messages: u64,
    pub messages_sent: u64,
    pub messages_failed: u64,
    pub messages_pending: u64,
    pub by_channel: ChannelStats,
    pub by_site: Vec<SiteStats>,
}

#[derive(Serialize)]
pub struct ChannelStats {
    pub email: u64,
    pub telegram: u64,
    pub sms: u64,
}

#[derive(Serialize)]
pub struct SiteStats {
    pub site_id: String,
    pub total: u64,
    pub sent: u64,
    pub failed: u64,
}

pub async fn get_stats(
    State(_state): State<Arc<AppState>>,
) -> Result<Json<StatsResponse>, StatusCode> {
    // TODO: Implement actual statistics gathering
    Ok(Json(StatsResponse {
        total_messages: 0,
        messages_sent: 0,
        messages_failed: 0,
        messages_pending: 0,
        by_channel: ChannelStats {
            email: 0,
            telegram: 0,
            sms: 0,
        },
        by_site: vec![],
    }))
}
