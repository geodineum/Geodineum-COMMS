//! GSD-COMMS: Notification Daemon for GSD
//!
//! Main entry point for the notification daemon.

use clap::Parser;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use gsd_comms::{
    config::{Cli, Command, Config},
    consumer::{SiteDiscovery, StreamConsumer},
    error::Result,
    filters::SpamFilter,
    retry::RetryManager,
    router::MessageDispatcher,
    settings::SettingsStore,
    templates::TemplateRenderer,
    ChannelRegistry,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse CLI arguments
    let cli = Cli::parse();

    // Initialize logging
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&cli.log_level));

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .init();

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "Starting GSD-COMMS"
    );

    // Load configuration
    let config = Config::load(&cli.config)?;

    // Handle subcommands
    match cli.command {
        Some(Command::Start) | None => {
            run_daemon(&cli, &config).await?;
        }
        Some(Command::Stop) => {
            info!("Sending stop signal...");
            // TODO: Implement proper stop signal
            println!("Stop not yet implemented - use systemctl stop gsd-comms");
        }
        Some(Command::Status) => {
            info!("Checking status...");
            // TODO: Implement status check
            println!("Status not yet implemented - use systemctl status gsd-comms");
        }
        Some(Command::Test { ref site_id, ref channel }) => {
            run_test(&cli, &config, site_id, channel).await?;
        }
        Some(Command::Encrypt { value }) => {
            // TODO: Implement encryption
            println!("Encryption not yet implemented");
            println!("Value would be encrypted: {}", "*".repeat(value.len()));
        }
    }

    Ok(())
}

async fn run_daemon(cli: &Cli, config: &Config) -> anyhow::Result<()> {
    info!(
        host = %cli.redis_host,
        port = cli.redis_port,
        environment = %cli.environment,
        "Connecting to ValKey"
    );

    // Build ValKey connection string
    let redis_url = if let Some(ref auth) = cli.redis_auth {
        format!(
            "redis://{}:{}@{}:{}/",
            cli.redis_user, auth, cli.redis_host, cli.redis_port
        )
    } else {
        format!("redis://{}:{}/", cli.redis_host, cli.redis_port)
    };

    // Connect to ValKey
    let client = redis::Client::open(redis_url)?;
    let conn = client.get_multiplexed_tokio_connection().await?;

    info!("Connected to ValKey");

    // Initialize components
    let template_renderer = Arc::new(TemplateRenderer::new(&cli.template_dir)?);
    let channel_registry = Arc::new(ChannelRegistry::new(template_renderer.clone()));
    let settings_store = Arc::new(SettingsStore::new(conn.clone()));
    let retry_manager = Arc::new(RetryManager::new(conn.clone()));

    // Initialize site discovery
    let site_discovery = SiteDiscovery::new(conn.clone(), cli.environment.clone());

    // Initialize stream consumer
    let node_id = cli.node_id.clone().unwrap_or_else(|| {
        hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "default".to_string())
    });

    let mut consumer = StreamConsumer::new(conn.clone(), site_discovery, node_id)
        .with_block_ms(config.consumer.block_ms)
        .with_batch_size(config.consumer.batch_size);

    // Initialize consumer groups
    consumer.initialize_groups().await?;

    // Create message dispatcher
    let dispatcher = MessageDispatcher::new(channel_registry.clone(), settings_store.clone());

    // Create spam filter
    let spam_filter = if config.spam_filter.enabled {
        Some(SpamFilter::default())
    } else {
        None
    };

    info!(
        api_bind = %cli.api_bind,
        api_port = cli.api_port,
        "GSD-COMMS daemon started"
    );

    // Main processing loop
    loop {
        // Read messages from streams
        match consumer.read_messages().await {
            Ok(messages) => {
                for (entry_id, message) in messages {
                    // Check spam
                    if let Some(ref filter) = spam_filter {
                        let result = filter.check(&message);
                        if result.is_spam {
                            warn!(
                                message_id = %message.id,
                                score = result.score,
                                reasons = ?result.reasons,
                                "Skipping spam message"
                            );
                            // ACK to remove from stream
                            let stream_key = format!(
                                "{}:gsd:comms:{}",
                                message.site_id, cli.environment
                            );
                            consumer.ack_message(&stream_key, &entry_id).await.ok();
                            continue;
                        }
                    }

                    // Skip test messages in production
                    if message.message_type == "test" && cli.environment == "production" {
                        continue;
                    }

                    // Skip system messages
                    if message.message_type == "system" {
                        let stream_key = format!(
                            "{}:gsd:comms:{}",
                            message.site_id, cli.environment
                        );
                        consumer.ack_message(&stream_key, &entry_id).await.ok();
                        continue;
                    }

                    // Dispatch message
                    let stream_key = format!(
                        "{}:gsd:comms:{}",
                        message.site_id, cli.environment
                    );

                    match dispatcher.dispatch(&message).await {
                        Ok(result) => {
                            if result.is_success() || result.skipped_channels.len() > 0 {
                                // ACK successful dispatch
                                consumer.ack_message(&stream_key, &entry_id).await.ok();
                                retry_manager
                                    .record_success(&message.site_id, &message.id)
                                    .await
                                    .ok();
                            } else if result.has_retryable_failures() {
                                // Schedule retry
                                let failed_channels: Vec<String> = result
                                    .failed_channels
                                    .iter()
                                    .map(|e| e.channel.clone())
                                    .collect();
                                let error_msg = result
                                    .failed_channels
                                    .first()
                                    .map(|e| e.error.to_string())
                                    .unwrap_or_default();

                                retry_manager
                                    .record_failure(
                                        &message.id,
                                        &message.site_id,
                                        &stream_key,
                                        &entry_id,
                                        &error_msg,
                                        failed_channels,
                                        None,
                                    )
                                    .await
                                    .ok();
                            } else {
                                // Non-retryable failure, ACK anyway
                                consumer.ack_message(&stream_key, &entry_id).await.ok();
                            }
                        }
                        Err(e) => {
                            error!(
                                message_id = %message.id,
                                error = %e,
                                "Failed to dispatch message"
                            );
                        }
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "Failed to read messages");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }

        // Process retries
        if let Ok(due_retries) = retry_manager.get_due_retries().await {
            for retry_state in due_retries {
                info!(
                    message_id = %retry_state.message_id,
                    attempt = retry_state.attempts,
                    "Retrying message"
                );
                // TODO: Implement retry dispatch
            }
        }
    }
}

async fn run_test(
    cli: &Cli,
    config: &Config,
    site_id: &str,
    channel: &str,
) -> anyhow::Result<()> {
    info!(
        site_id = %site_id,
        channel = %channel,
        "Testing notification channels"
    );

    // Build connection
    let redis_url = if let Some(ref auth) = cli.redis_auth {
        format!(
            "redis://{}:{}@{}:{}/",
            cli.redis_user, auth, cli.redis_host, cli.redis_port
        )
    } else {
        format!("redis://{}:{}/", cli.redis_host, cli.redis_port)
    };

    let client = redis::Client::open(redis_url)?;
    let conn = client.get_multiplexed_tokio_connection().await?;

    // Initialize components
    let template_renderer = Arc::new(TemplateRenderer::new(&cli.template_dir)?);
    let channel_registry = Arc::new(ChannelRegistry::new(template_renderer));
    let settings_store = Arc::new(SettingsStore::new(conn));

    // Load settings
    let settings = settings_store.get_settings(site_id).await?;

    match settings {
        Some(s) => {
            if !s.enabled {
                println!("Site notifications are disabled");
                return Ok(());
            }

            let channels_to_test = if channel == "all" {
                channel_registry.list()
            } else {
                vec![channel]
            };

            for ch in channels_to_test {
                print!("Testing {} channel... ", ch);

                if let Some(ch_impl) = channel_registry.get(ch) {
                    let ch_config = match ch {
                        "email" => s.channels.email.clone(),
                        "telegram" => s.channels.telegram.clone(),
                        "sms" => s.channels.sms.clone(),
                        _ => None,
                    };

                    match ch_config {
                        Some(config) if config.enabled => {
                            match ch_impl.validate_config(&config) {
                                Ok(_) => println!("OK (config valid)"),
                                Err(e) => println!("FAILED: {}", e),
                            }
                        }
                        Some(_) => println!("DISABLED"),
                        None => println!("NOT CONFIGURED"),
                    }
                } else {
                    println!("UNKNOWN CHANNEL");
                }
            }
        }
        None => {
            println!("No settings found for site: {}", site_id);
            println!("Create settings first using the admin API");
        }
    }

    Ok(())
}
