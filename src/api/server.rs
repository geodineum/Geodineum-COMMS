//! Axum-based API server

use axum::{
    routing::{get, post, put},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

use crate::error::Result;
use crate::settings::SettingsStore;
use crate::ChannelRegistry;

use super::routes;

/// API server state shared across handlers
pub struct AppState {
    pub settings_store: Arc<SettingsStore>,
    pub channel_registry: Arc<ChannelRegistry>,
}

/// API server for admin dashboard
pub struct ApiServer {
    addr: SocketAddr,
    router: Router,
}

impl ApiServer {
    pub fn new(
        bind: &str,
        port: u16,
        settings_store: Arc<SettingsStore>,
        channel_registry: Arc<ChannelRegistry>,
    ) -> Self {
        let state = Arc::new(AppState {
            settings_store,
            channel_registry,
        });

        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        let router = Router::new()
            // Health endpoints
            .route("/api/health", get(routes::health::health_check))
            // Site management
            .route("/api/sites", get(routes::sites::list_sites))
            .route("/api/sites/:site_id", get(routes::sites::get_site))
            .route("/api/sites/:site_id", put(routes::sites::update_site))
            .route("/api/sites/:site_id/test", post(routes::sites::test_channel))
            // Message history
            .route("/api/messages", get(routes::messages::list_messages))
            .route("/api/messages/:id", get(routes::messages::get_message))
            .route("/api/messages/:id/retry", post(routes::messages::retry_message))
            // Statistics
            .route("/api/stats", get(routes::stats::get_stats))
            // Dashboard (Htmx)
            .route("/", get(routes::dashboard::index))
            .route("/dashboard", get(routes::dashboard::index))
            .route("/dashboard/sites", get(routes::dashboard::sites))
            .route("/dashboard/messages", get(routes::dashboard::messages))
            .route("/dashboard/settings", get(routes::dashboard::settings))
            .layer(cors)
            .with_state(state);

        let addr: SocketAddr = format!("{}:{}", bind, port)
            .parse()
            .expect("Invalid bind address");

        Self { addr, router }
    }

    /// Run the API server
    pub async fn run(self) -> Result<()> {
        info!(addr = %self.addr, "Starting API server");

        let listener = tokio::net::TcpListener::bind(self.addr)
            .await
            .map_err(|e| crate::error::CommsError::Internal(e.to_string()))?;

        axum::serve(listener, self.router)
            .await
            .map_err(|e| crate::error::CommsError::Internal(e.to_string()))?;

        Ok(())
    }
}
