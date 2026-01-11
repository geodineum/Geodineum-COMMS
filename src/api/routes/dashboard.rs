//! Dashboard HTML routes
//!
//! Provides a minimal status dashboard. Full configuration is handled
//! via the gCore WordPress admin module (gCore/Modules/Comms).

use axum::{
    extract::State,
    response::{Html, IntoResponse},
};
use std::sync::Arc;

use crate::api::server::AppState;

const STATUS_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>GSD-COMMS Status</title>
    <style>
        * { box-sizing: border-box; margin: 0; padding: 0; }
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: linear-gradient(135deg, #1e3a5f 0%, #0f1f32 100%);
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
            color: #fff;
        }
        .container {
            max-width: 600px;
            padding: 2rem;
            text-align: center;
        }
        .logo {
            font-size: 3rem;
            margin-bottom: 1rem;
        }
        h1 {
            font-size: 2rem;
            margin-bottom: 0.5rem;
            font-weight: 600;
        }
        .version {
            opacity: 0.7;
            font-size: 0.9rem;
            margin-bottom: 2rem;
        }
        .status-card {
            background: rgba(255, 255, 255, 0.1);
            border-radius: 12px;
            padding: 2rem;
            margin-bottom: 2rem;
            backdrop-filter: blur(10px);
        }
        .status-indicator {
            display: inline-flex;
            align-items: center;
            gap: 0.5rem;
            font-size: 1.25rem;
            margin-bottom: 1rem;
        }
        .status-dot {
            width: 12px;
            height: 12px;
            border-radius: 50%;
            background: #10b981;
            animation: pulse 2s infinite;
        }
        @keyframes pulse {
            0%, 100% { opacity: 1; }
            50% { opacity: 0.5; }
        }
        .stats {
            display: grid;
            grid-template-columns: repeat(3, 1fr);
            gap: 1rem;
            margin-top: 1.5rem;
        }
        .stat {
            padding: 1rem;
            background: rgba(255, 255, 255, 0.05);
            border-radius: 8px;
        }
        .stat-value {
            font-size: 1.5rem;
            font-weight: bold;
        }
        .stat-label {
            font-size: 0.75rem;
            opacity: 0.7;
            text-transform: uppercase;
        }
        .actions {
            margin-top: 2rem;
        }
        .btn {
            display: inline-block;
            padding: 0.75rem 1.5rem;
            background: #2563eb;
            color: white;
            text-decoration: none;
            border-radius: 8px;
            font-weight: 500;
            transition: background 0.2s;
        }
        .btn:hover {
            background: #1d4ed8;
        }
        .note {
            margin-top: 2rem;
            font-size: 0.875rem;
            opacity: 0.7;
        }
        .api-info {
            margin-top: 2rem;
            padding: 1rem;
            background: rgba(255, 255, 255, 0.05);
            border-radius: 8px;
            text-align: left;
            font-family: monospace;
            font-size: 0.8rem;
        }
        .api-info h3 {
            font-size: 0.9rem;
            margin-bottom: 0.5rem;
        }
        .api-info code {
            display: block;
            padding: 0.25rem 0;
            opacity: 0.8;
        }
    </style>
</head>
<body>
    <div class="container">
        <div class="logo">📬</div>
        <h1>GSD-COMMS</h1>
        <p class="version">Notification Daemon v0.1.0</p>

        <div class="status-card">
            <div class="status-indicator">
                <span class="status-dot"></span>
                <span>Daemon Running</span>
            </div>
            <div class="stats" id="stats">
                <div class="stat">
                    <div class="stat-value">-</div>
                    <div class="stat-label">Sites</div>
                </div>
                <div class="stat">
                    <div class="stat-value">-</div>
                    <div class="stat-label">Pending</div>
                </div>
                <div class="stat">
                    <div class="stat-value">-</div>
                    <div class="stat-label">Sent (24h)</div>
                </div>
            </div>
        </div>

        <div class="actions">
            <p style="margin-bottom: 1rem;">Configure notifications via WordPress Admin:</p>
            <a href="/wp-admin/admin.php?page=gcore-comms" class="btn">
                Open Dashboard
            </a>
        </div>

        <div class="api-info">
            <h3>API Endpoints</h3>
            <code>GET /api/health - Health check</code>
            <code>GET /api/sites - List configured sites</code>
            <code>GET /api/messages - Message history</code>
            <code>GET /api/stats - Statistics</code>
        </div>

        <p class="note">
            Full configuration available in WordPress Admin &rarr; gCore &rarr; Notifications
        </p>
    </div>

    <script>
        // Fetch stats on load
        fetch('/api/stats')
            .then(r => r.json())
            .then(data => {
                if (data) {
                    const stats = document.getElementById('stats');
                    stats.innerHTML = `
                        <div class="stat">
                            <div class="stat-value">${data.sites_count || 0}</div>
                            <div class="stat-label">Sites</div>
                        </div>
                        <div class="stat">
                            <div class="stat-value">${data.pending || 0}</div>
                            <div class="stat-label">Pending</div>
                        </div>
                        <div class="stat">
                            <div class="stat-value">${data.sent_24h || 0}</div>
                            <div class="stat-label">Sent (24h)</div>
                        </div>
                    `;
                }
            })
            .catch(() => {});
    </script>
</body>
</html>"#;

fn render_status_page() -> Html<&'static str> {
    Html(STATUS_HTML)
}

/// Main dashboard - shows daemon status
pub async fn index(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    render_status_page()
}

/// Redirect to WordPress admin for site management
pub async fn sites(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    render_status_page()
}

/// Redirect to WordPress admin for message history
pub async fn messages(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    render_status_page()
}

/// Redirect to WordPress admin for settings
pub async fn settings(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    render_status_page()
}
