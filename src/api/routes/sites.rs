//! Site management endpoints

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::api::server::AppState;
use crate::settings::SiteSettings;

#[derive(Serialize)]
pub struct SiteListResponse {
    pub sites: Vec<String>,
}

#[derive(Serialize)]
pub struct SiteResponse {
    pub settings: Option<SiteSettings>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
pub struct TestChannelRequest {
    pub channel: String,
}

#[derive(Serialize)]
pub struct TestResult {
    pub channel: String,
    pub success: bool,
    pub message: String,
}

pub async fn list_sites(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SiteListResponse>, StatusCode> {
    match state.settings_store.list_sites().await {
        Ok(sites) => Ok(Json(SiteListResponse { sites })),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub async fn get_site(
    State(state): State<Arc<AppState>>,
    Path(site_id): Path<String>,
) -> Result<Json<SiteResponse>, StatusCode> {
    match state.settings_store.get_settings(&site_id).await {
        Ok(settings) => Ok(Json(SiteResponse {
            settings,
            error: None,
        })),
        Err(e) => Ok(Json(SiteResponse {
            settings: None,
            error: Some(e.to_string()),
        })),
    }
}

pub async fn update_site(
    State(state): State<Arc<AppState>>,
    Path(site_id): Path<String>,
    Json(mut settings): Json<SiteSettings>,
) -> Result<Json<SiteResponse>, StatusCode> {
    // Ensure site_id matches
    settings.site_id = site_id;

    match state.settings_store.save_settings(&settings).await {
        Ok(_) => Ok(Json(SiteResponse {
            settings: Some(settings),
            error: None,
        })),
        Err(e) => Ok(Json(SiteResponse {
            settings: None,
            error: Some(e.to_string()),
        })),
    }
}

pub async fn test_channel(
    State(state): State<Arc<AppState>>,
    Path(site_id): Path<String>,
    Json(request): Json<TestChannelRequest>,
) -> Result<Json<TestResult>, StatusCode> {
    // Get settings
    let settings = match state.settings_store.get_settings(&site_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Ok(Json(TestResult {
                channel: request.channel,
                success: false,
                message: "Site not found".to_string(),
            }))
        }
        Err(e) => {
            return Ok(Json(TestResult {
                channel: request.channel,
                success: false,
                message: e.to_string(),
            }))
        }
    };

    // Get channel
    let channel = match state.channel_registry.get(&request.channel) {
        Some(c) => c,
        None => {
            return Ok(Json(TestResult {
                channel: request.channel,
                success: false,
                message: "Unknown channel".to_string(),
            }))
        }
    };

    // Get channel config
    let config = match request.channel.as_str() {
        "email" => settings.channels.email,
        "telegram" => settings.channels.telegram,
        "sms" => settings.channels.sms,
        _ => None,
    };

    match config {
        Some(cfg) => match channel.validate_config(&cfg) {
            Ok(_) => Ok(Json(TestResult {
                channel: request.channel,
                success: true,
                message: "Configuration valid".to_string(),
            })),
            Err(e) => Ok(Json(TestResult {
                channel: request.channel,
                success: false,
                message: e.to_string(),
            })),
        },
        None => Ok(Json(TestResult {
            channel: request.channel,
            success: false,
            message: "Channel not configured".to_string(),
        })),
    }
}
