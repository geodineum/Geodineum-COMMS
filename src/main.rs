//! Geodineum-COMMS: Notification Daemon for Geodineum
//!
//! Main entry point for the notification daemon.

use clap::Parser;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use geodineum_comms::{
    cli::{self, OutputFormat, TablePrinter},
    config::{Cli, Command, Config},
    consumer::{SiteDiscovery, StreamConsumer},
    filters::SpamFilter,
    inbound::{
        CommandAction, CommandRouter, ConversationState, PollRequest,
        InferenceChainConfig, TelegramReceiver, WorkflowDispatcher,
        send_processing_indicator, spawn_response_poller,
    },
    persistence::{BackoffTracker, MessageStatus, MessageStore, SpamRetentionPolicy},
    retry::RetryManager,
    router::MessageDispatcher,
    settings::SettingsStore,
    templates::TemplateRenderer,
    ChannelRegistry, CommsMessage,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Per-site dispatch rate limiter (sliding window counter).
///
/// Prevents flooding by enforcing a maximum number of dispatches per site
/// within a configurable time window. Defaults: 100 dispatches per 60 seconds.
///
/// bounded-size hash map. Pre-fix, distinct
/// per-message site_id values caused linear memory blow-up because
/// inactive-site entries were never evicted. Post-fix: every try_acquire
/// opportunistically prunes entries whose timestamps have all aged past
/// the window cutoff, and a hard cap (MAX_TRACKED_SITES) caps the map
/// size even under adversarial load. an earlier hardening pass's site_id regex in
/// stream_reader.rs also defends this path — invalid site_ids are
/// rejected upstream before reaching try_acquire, so the remaining
/// blow-up vector is "lots of distinct valid site_ids" which the hard
/// cap addresses.
struct DispatchRateLimiter {
    /// Per-site: list of dispatch timestamps within the current window
    windows: HashMap<String, Vec<Instant>>,
    /// Maximum dispatches per window
    max_per_window: usize,
    /// Window duration
    window_duration: Duration,
    /// Global counter for logging
    total_limited: u64,
}

/// Upper bound on distinct site_ids tracked simultaneously. Real-world
/// constellations run in the low hundreds; 1024 leaves ~5-10× headroom
/// while capping memory. Exceeding this bound in practice means either
/// a real operational need (raise the constant) or adversarial pressure
/// from a caller bypassing the site_id regex (investigate upstream).
const MAX_TRACKED_SITES: usize = 1024;

impl DispatchRateLimiter {
    fn new(max_per_window: usize, window_secs: u64) -> Self {
        Self {
            windows: HashMap::new(),
            max_per_window,
            window_duration: Duration::from_secs(window_secs),
            total_limited: 0,
        }
    }

    /// Check if a dispatch is allowed for this site. If yes, records it.
    fn try_acquire(&mut self, site_id: &str) -> bool {
        let now = Instant::now();
        let cutoff = now - self.window_duration;

        // Drop entries whose entire window has aged out. Keeps the map
        // bounded to sites active in the recent window under normal load.
        self.windows.retain(|_, ts| ts.iter().any(|t| *t > cutoff));

        // Hard cap: if adversary or bug pushed us past the bound AND the
        // caller is a previously-unseen site, reject rather than insert.
        // Existing-site callers continue to flow (their entry is already
        // in the map, so .entry() below won't grow it).
        if self.windows.len() >= MAX_TRACKED_SITES
            && !self.windows.contains_key(site_id)
        {
            self.total_limited += 1;
            return false;
        }

        let timestamps = self.windows.entry(site_id.to_string()).or_default();

        // Per-entry prune
        timestamps.retain(|t| *t > cutoff);

        if timestamps.len() >= self.max_per_window {
            self.total_limited += 1;
            false
        } else {
            timestamps.push(now);
            true
        }
    }
}

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
        "Starting Geodineum-COMMS"
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
            println!("Stop not yet implemented - use systemctl stop geodineum-comms");
        }
        Some(Command::Status) => {
            info!("Checking status...");
            // TODO: Implement status check
            println!("Status not yet implemented - use systemctl status geodineum-comms");
        }
        Some(Command::Test { ref site_id, ref channel }) => {
            run_test(&cli, &config, site_id, channel).await?;
        }
        Some(Command::Encrypt { value }) => {
            // TODO: Implement encryption
            println!("Encryption not yet implemented");
            println!("Value would be encrypted: {}", "*".repeat(value.len()));
        }
        Some(Command::Sites) => {
            run_sites_command()?;
        }
        Some(Command::Messages {
            site_id,
            status,
            message_type,
            limit,
            format,
            verbose,
        }) => {
            run_messages_command(
                site_id.as_deref(),
                status.as_deref(),
                message_type.as_deref(),
                limit,
                &format,
                verbose,
            )?;
        }
        Some(Command::Stats { site_id, format }) => {
            run_stats_command(site_id.as_deref(), &format)?;
        }
        Some(Command::Message {
            site_id,
            message_id,
            format,
        }) => {
            run_message_command(&site_id, &message_id, &format)?;
        }
        Some(Command::Retry { site_id, message_id }) => {
            run_retry_command(&site_id, &message_id)?;
        }
        Some(Command::Cleanup {
            site_id,
            max_age_days,
            delete_spam,
            vacuum,
            dry_run,
        }) => {
            run_cleanup_command(
                site_id.as_deref(),
                max_age_days,
                delete_spam,
                vacuum,
                dry_run,
                &config,
            )?;
        }
        Some(Command::DbStats { site_id, format }) => {
            run_db_stats_command(site_id.as_deref(), &format)?;
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
    // an earlier hardening pass: chain `.with_settings(...)` so operator's YAML retry config
    // (max_attempts / base_delay_secs / max_delay_secs) actually takes
    // effect. Previously `RetryManager::new` was used alone and the
    // operator-provided values were silently dropped — the manager
    // ran on hardcoded RetrySettings::default() forever.
    let retry_settings = geodineum_comms::settings::RetrySettings {
        max_attempts: config.retry.max_attempts,
        base_delay_secs: config.retry.base_delay_secs,
        max_delay_secs: config.retry.max_delay_secs,
    };
    let retry_manager = Arc::new(
        RetryManager::new(conn.clone()).with_settings(retry_settings)
    );

    // Initialize SQLite message store for archiving
    let data_dir = get_data_dir();
    let message_store = match MessageStore::new(&data_dir) {
        Ok(store) => {
            info!(data_dir = %data_dir, "Message store initialized");
            Some(store)
        }
        Err(e) => {
            warn!(error = %e, "Failed to initialize message store - archiving disabled");
            None
        }
    };

    // Initialize backoff tracker for archive failures
    let mut backoff_tracker = BackoffTracker::new();

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
        .with_batch_size(config.consumer.batch_size)
        .with_discovery_interval(config.consumer.discovery_interval_secs);

    // Initialize consumer groups
    consumer.initialize_groups().await?;

    // Create message dispatcher
    let dispatcher = MessageDispatcher::new(
        channel_registry.clone(),
        settings_store.clone(),
        cli.allow_nonprod_send,
    );

    // Create spam filter: built-in blocklists extended by the YAML config
    // (spam_filter.keywords_blocklist / ip_blocklist were previously
    // accepted but never wired — a silently inert security knob).
    let spam_filter = if config.spam_filter.enabled {
        Some(SpamFilter::with_extra_blocklists(
            &config.spam_filter.keywords_blocklist,
            &config.spam_filter.ip_blocklist,
        ))
    } else {
        None
    };

    // Initialize dispatch rate limiter (100 dispatches per 60s per site)
    let mut rate_limiter = DispatchRateLimiter::new(100, 60);

    // ── Inbound: Telegram receiver + command router ──────────────────────
    let cancel_token = CancellationToken::new();
    let default_pipeline = config
        .inbound
        .as_ref()
        .and_then(|i| i.default_pipeline.clone())
        .unwrap_or_else(|| "sysadmin".to_string());
    let mut conv_state = ConversationState::new(conn.clone(), default_pipeline.clone());

    // Configure command router with the inference chain + workflow dispatch
    let chain_enabled = config
        .inbound
        .as_ref()
        .and_then(|i| i.inference_chain_enabled)
        .unwrap_or(false);
    let chain_config = InferenceChainConfig::from_env();

    let inbound_site_id_for_wf = config
        .inbound
        .as_ref()
        .and_then(|i| i.site_id.clone())
        .unwrap_or_else(|| "geodine".to_string());
    let workflow_dispatcher = WorkflowDispatcher::new(
        &inbound_site_id_for_wf,
        &cli.environment,
    );

    let command_router = CommandRouter::new()
        .with_inference_chain(chain_enabled)
        .with_workflow_dispatcher(workflow_dispatcher);

    // Spawn Telegram inbound poller for each site that has Telegram configured
    // with admin_ids. We check settings on startup; new sites discovered later
    // require a daemon restart for INBOUND only — outbound picks them up via
    // the periodic re-discovery (consumer.discovery_interval_secs).
    let inbound_site_id = config
        .inbound
        .as_ref()
        .and_then(|i| i.site_id.clone())
        .unwrap_or_else(|| "geodine".to_string());

    let inbound_bot_token: Option<String> = config
        .inbound
        .as_ref()
        .and_then(|i| i.bot_token.clone());

    if let Some(ref inbound_cfg) = config.inbound {
        if let Some(ref bot_token) = inbound_cfg.bot_token {
            // an earlier hardening pass: preserve the Option<Vec> shape rather than
            // `.unwrap_or_default()` flattening, so the receiver can
            // distinguish "not configured" (None) from "configured-empty"
            // (Some(vec![]) → fail-closed).
            let admin_ids: Option<Vec<i64>> = inbound_cfg.admin_ids.clone();

            let mut receiver = TelegramReceiver::new(
                bot_token.clone(),
                conn.clone(),
                inbound_site_id.clone(),
                cli.environment.clone(),
                admin_ids,
            );

            let child_cancel = cancel_token.clone();
            tokio::spawn(async move {
                receiver.start_polling(child_cancel).await;
            });

            info!("Telegram inbound polling started for site {}", inbound_site_id);
        }
    }

    // Inference timeout from env (INFERENCE_TIMEOUT). Default 7200s (2h): the
    // local 35B (aurelius) generates a long reply at ~1 tok/s, so a full
    // max_tokens response is a 40-60 min generation. The old 3000s (50 min)
    // default abandoned replies that were still cooking (2026-07-26) — the
    // operator would rather wait than lose the answer. Raise the env var if a
    // pipeline can legitimately run longer.
    let inference_timeout: u64 = std::env::var("INFERENCE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7200);

    // Inbound stream consumer — reads from {site_id}:gnode:comms:inbound:{env}
    // (brace-literal hash-tag form for cluster-mode co-location).
    let inbound_stream_key = format!(
        "{{{}}}:gnode:comms:inbound:{}",
        inbound_site_id, cli.environment
    );
    // Ensure consumer group exists for inbound stream
    {
        let result: std::result::Result<(), redis::RedisError> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(&inbound_stream_key)
            .arg("geodineum_comms_inbound")
            .arg("0")
            .arg("MKSTREAM")
            .query_async(&mut conn.clone())
            .await;
        match result {
            Ok(_) => info!("Created inbound consumer group"),
            Err(e) if e.to_string().contains("BUSYGROUP") => {}
            Err(e) => warn!(error = %e, "Failed to create inbound consumer group"),
        }
    }

    // Clone conn for inbound reads
    let mut inbound_conn = conn.clone();

    // Publish COMMS message schemas to ValKey for component/agent discovery
    geodineum_comms::inbound::publish_comms_schemas(
        &mut conn.clone(),
        &inbound_site_id,
        &cli.environment,
    )
    .await;

    info!("Geodineum-COMMS daemon started (bidirectional mode)");

    // Track last retry processing time
    let mut last_retry_check = Instant::now();
    let retry_check_interval = Duration::from_secs(30);

    // Dedicated ValKey handle for mirroring dispatch status back to the
    // dashboard (see writeback_status). Cheap clone of the multiplexed pipe.
    let mut status_conn = conn.clone();

    // Receipt producer identity (T-E). COMMS signs with its OWN key: `signer`
    // identifies the producer, and it runs as geodineum-comms so it cannot read
    // the daemon's key (0600 gnode:gnode) in any case. Generated 0600 on first
    // use; the pubkey is published so verifiers can resolve the fingerprint.
    // Failure here is non-fatal and LOUD: no context means no receipts, which
    // must be visible rather than inferred from an empty stream.
    {
        let key_path = geodineum_comms::receipt::default_signer_path();
        match geodineum_comms::receipt::load_or_generate_signer(&key_path) {
            Ok(signer) => {
                let ns = std::env::var("GNODE_TOPOLOGY_NAMESPACE")
                    .unwrap_or_else(|_| "geodineum".to_string());
                let mut pk_conn = conn.clone();
                match geodineum_comms::receipt::publish_pubkey(&mut pk_conn, &ns, &signer).await {
                    Ok(()) => info!(
                        signer = %signer.signer_id(), key = %key_path.display(),
                        "receipt signer ready; pubkey published"
                    ),
                    Err(e) => warn!(
                        error = %e,
                        "receipt pubkey publish FAILED — receipts will be written but \
                         nothing can verify them until the key is in the registry"
                    ),
                }
                geodineum_comms::receipt::init_receipt_context(
                    signer, "comms".to_string(), cli.environment.clone(),
                );
            }
            Err(e) => warn!(
                error = %e, key = %key_path.display(),
                "receipt signer unavailable — COMMS will emit NO receipts this run"
            ),
        }
    }

    // Liveness heartbeat for the operator dashboard (see write_heartbeat).
    // Write once up front so COMMS shows up immediately, then refresh on a
    // ~60s cadence from the loop tail.
    let mut heartbeat_conn = conn.clone();
    write_heartbeat(&mut heartbeat_conn, &cli.environment).await;
    let mut last_heartbeat = Instant::now();

    // Main processing loop
    loop {
        // Read messages from streams
        match consumer.read_messages().await {
            Ok(messages) => {
                for (entry_id, message) in messages {
                    let stream_key = format!(
                        "{{{}}}:gnode:comms:{}",
                        message.site_id, cli.environment
                    );

                    // Check if site is in backoff (SQLite archive failures)
                    if backoff_tracker.is_in_backoff(&message.site_id) {
                        if let Some(remaining) = backoff_tracker.remaining_backoff(&message.site_id) {
                            tracing::debug!(
                                site_id = %message.site_id,
                                remaining_secs = remaining.as_secs(),
                                "Site in backoff, skipping message"
                            );
                        }
                        // Don't ACK - message will be reprocessed after backoff
                        continue;
                    }

                    // Dedup: skip if already sent
                    if let Some(ref store) = message_store {
                        if store.is_already_sent(&message.site_id, &message.id) {
                            tracing::debug!(
                                message_id = %message.id,
                                "Message already sent — ACKing duplicate"
                            );
                            consumer.ack_message(&stream_key, &entry_id).await.ok();
                            continue;
                        }
                    }

                    // Check spam
                    let spam_result = spam_filter.as_ref().map(|f| f.check(&message));
                    let is_spam = spam_result.as_ref().map(|r| r.is_spam).unwrap_or(false);

                    if is_spam {
                        let result = spam_result.unwrap();
                        warn!(
                            message_id = %message.id,
                            score = result.score,
                            reasons = ?result.reasons,
                            "Marking message as spam"
                        );

                        // Archive as spam, then ACK
                        if let Some(ref store) = message_store {
                            match archive_message(store, &message, &entry_id, MessageStatus::Spam) {
                                Ok(_) => {
                                    backoff_tracker.record_success(&message.site_id);
                                    consumer.ack_message(&stream_key, &entry_id).await.ok();
                                }
                                Err(e) => {
                                    backoff_tracker.record_failure(&message.site_id, &e.to_string());
                                    // Don't ACK - will retry after backoff
                                }
                            }
                        } else {
                            consumer.ack_message(&stream_key, &entry_id).await.ok();
                        }
                        continue;
                    }

                    // Settings-reload control message: drop the cached settings
                    // for this site so the next message re-reads
                    // {site}:comms:config. Producer: gNode-Client on every
                    // settings save/delete. Rides this durable consumer-group
                    // stream (not pub/sub) so a signal issued while the daemon is
                    // down is still applied on restart. Never dispatched.
                    if message.message_type == "settings.reload" {
                        settings_store.invalidate_cache(&message.site_id);
                        tracing::info!(
                            site_id = %message.site_id,
                            "Settings cache invalidated (reload signal)"
                        );
                        consumer.ack_message(&stream_key, &entry_id).await.ok();
                        continue;
                    }

                    // Skip test messages in production
                    if message.message_type == "test" && cli.environment == "production" {
                        consumer.ack_message(&stream_key, &entry_id).await.ok();
                        continue;
                    }

                    // Skip system messages
                    if message.message_type == "system" {
                        consumer.ack_message(&stream_key, &entry_id).await.ok();
                        continue;
                    }

                    // Rate limit check
                    if !rate_limiter.try_acquire(&message.site_id) {
                        warn!(
                            message_id = %message.id,
                            site_id = %message.site_id,
                            "Rate limit exceeded — skipping dispatch"
                        );
                        // Don't ACK — will be retried after window resets
                        continue;
                    }

                    // Reply-correlation: if this outbound message has reply_options,
                    // register context so operator replies can be routed back
                    if let Some(ref metadata) = Some(&message.metadata) {
                        let has_reply_options = metadata.get("reply_options").is_some();
                        let context_id = metadata
                            .get("context_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let component = metadata
                            .get("component")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let callback_stream = metadata
                            .get("callback_stream")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let chat_id_meta = metadata
                            .get("chat_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        if has_reply_options
                            && !context_id.is_empty()
                            && !component.is_empty()
                            && !chat_id_meta.is_empty()
                        {
                            let reply_options: Vec<String> = metadata
                                .get("reply_options")
                                .and_then(|v| serde_json::from_value(v.clone()).ok())
                                .unwrap_or_default();

                            conv_state
                                .track_context(
                                    &message.site_id,
                                    chat_id_meta,
                                    context_id,
                                    component,
                                    &reply_options,
                                    callback_stream,
                                    &message.id,
                                )
                                .await
                                .ok();
                        }
                    }

                    // Dispatch message
                    match dispatcher.dispatch(&message).await {
                        Ok(result) => {
                            // Determine final status
                            let status = if result.is_success() {
                                MessageStatus::Sent
                            } else if result.skipped_channels.len() > 0 && result.failed_channels.is_empty() {
                                MessageStatus::Skipped
                            } else if result.has_retryable_failures() {
                                MessageStatus::Failed
                            } else if !result.failed_channels.is_empty() {
                                MessageStatus::PartialSent
                            } else {
                                MessageStatus::Sent
                            };

                            // Archive to SQLite first (transactional behavior)
                            let archive_success = if let Some(ref store) = message_store {
                                match archive_message(store, &message, &entry_id, status) {
                                    Ok(_) => {
                                        backoff_tracker.record_success(&message.site_id);
                                        true
                                    }
                                    Err(e) => {
                                        error!(
                                            message_id = %message.id,
                                            site_id = %message.site_id,
                                            error = %e,
                                            "Failed to archive message to SQLite"
                                        );
                                        backoff_tracker.record_failure(&message.site_id, &e.to_string());
                                        false
                                    }
                                }
                            } else {
                                true // No store configured, proceed
                            };

                            // Only ACK if archive succeeded (or no store)
                            if archive_success {
                                let err_s = result.failed_channels.first().map(|e| e.error.to_string());
                                writeback_status(&mut status_conn, &message.site_id, &entry_id, status.as_str(), 1, err_s.as_deref()).await;
                                if result.is_success() || result.skipped_channels.len() > 0 {
                                    consumer.ack_message(&stream_key, &entry_id).await.ok();
                                    retry_manager
                                        .record_success(&message.site_id, &message.id)
                                        .await
                                        .ok();
                                } else if result.has_retryable_failures() {
                                    // ACK the stream message (prevents infinite re-read)
                                    // and schedule a proper retry via RetryManager with backoff
                                    consumer.ack_message(&stream_key, &entry_id).await.ok();

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
                            // If archive failed, don't ACK - message stays in stream for retry
                        }
                        Err(e) => {
                            error!(
                                message_id = %message.id,
                                error = %e,
                                "Failed to dispatch message"
                            );
                            writeback_status(&mut status_conn, &message.site_id, &entry_id, "failed", 1, Some(&e.to_string())).await;
                            // ACK to prevent infinite retry loop, record failure with backoff
                            consumer.ack_message(&stream_key, &entry_id).await.ok();
                            retry_manager
                                .record_failure(
                                    &message.id,
                                    &message.site_id,
                                    &stream_key,
                                    &entry_id,
                                    &e.to_string(),
                                    vec![],
                                    None,
                                )
                                .await
                                .ok();
                        }
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "Failed to read messages");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }

        // Claim pending messages from dead consumers (XAUTOCLAIM)
        // Same guards as main path: backoff, dedup, spam, rate limit, retry count
        match consumer.claim_pending(config.consumer.idle_claim_ms).await {
            Ok(claimed) => {
                for (entry_id, message) in claimed {
                    let stream_key = format!(
                        "{{{}}}:gnode:comms:{}",
                        message.site_id, cli.environment
                    );

                    // Backoff check
                    if backoff_tracker.is_in_backoff(&message.site_id) {
                        continue;
                    }

                    // Dedup check
                    if let Some(ref store) = message_store {
                        if store.is_already_sent(&message.site_id, &message.id) {
                            consumer.ack_message(&stream_key, &entry_id).await.ok();
                            continue;
                        }
                    }

                    // Retry count check — don't process if max attempts exceeded
                    if let Ok(Some(retry_state)) = retry_manager.get_retry_state(&message.site_id, &message.id).await {
                        if retry_state.attempts >= config.retry.max_attempts {
                            warn!(
                                message_id = %message.id,
                                attempts = retry_state.attempts,
                                "Claimed message exceeded max retries — ACKing as failed"
                            );
                            if let Some(ref store) = message_store {
                                archive_message(store, &message, &entry_id, MessageStatus::Failed).ok();
                            }
                            writeback_status(&mut status_conn, &message.site_id, &entry_id, "failed", config.retry.max_attempts as i64, Some("exceeded max retries")).await;
                            consumer.ack_message(&stream_key, &entry_id).await.ok();
                            continue;
                        }
                    }

                    // Spam check
                    let is_spam = spam_filter.as_ref().map(|f| f.check(&message).is_spam).unwrap_or(false);
                    if is_spam {
                        if let Some(ref store) = message_store {
                            archive_message(store, &message, &entry_id, MessageStatus::Spam).ok();
                        }
                        consumer.ack_message(&stream_key, &entry_id).await.ok();
                        continue;
                    }

                    // Rate limit check
                    if !rate_limiter.try_acquire(&message.site_id) {
                        // Don't ACK — will be reclaimed next cycle when rate limit resets
                        continue;
                    }

                    info!(
                        message_id = %message.id,
                        site_id = %message.site_id,
                        "Processing claimed pending message"
                    );

                    match dispatcher.dispatch(&message).await {
                        Ok(result) => {
                            let status = if result.is_success() {
                                MessageStatus::Sent
                            } else {
                                MessageStatus::Failed
                            };
                            if let Some(ref store) = message_store {
                                archive_message(store, &message, &entry_id, status).ok();
                            }
                            writeback_status(&mut status_conn, &message.site_id, &entry_id, status.as_str(), 1, None).await;
                            consumer.ack_message(&stream_key, &entry_id).await.ok();
                            if result.is_success() {
                                retry_manager.record_success(&message.site_id, &message.id).await.ok();
                            }
                        }
                        Err(e) => {
                            warn!(
                                message_id = %message.id,
                                error = %e,
                                "Failed to dispatch claimed message"
                            );
                            // ACK + record failure with backoff (prevents infinite claim loop)
                            consumer.ack_message(&stream_key, &entry_id).await.ok();
                            retry_manager
                                .record_failure(
                                    &message.id,
                                    &message.site_id,
                                    &stream_key,
                                    &entry_id,
                                    &e.to_string(),
                                    vec![],
                                    None,
                                )
                                .await
                                .ok();
                        }
                    }
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "Failed to claim pending messages");
            }
        }

        // Process due retries (with exponential backoff from RetryManager)
        if last_retry_check.elapsed() >= retry_check_interval {
            last_retry_check = Instant::now();

            if let Ok(due_retries) = retry_manager.get_due_retries().await {
                for retry_state in due_retries {
                    // Rate limit check for retries too
                    if !rate_limiter.try_acquire(&retry_state.site_id) {
                        continue;
                    }

                    info!(
                        message_id = %retry_state.message_id,
                        attempt = retry_state.attempts,
                        channels = ?retry_state.failed_channels,
                        "Retrying failed message"
                    );

                    // Re-fetch the entry from stream history (retained after
                    // XACK until trimmed) and re-dispatch ONLY the channels
                    // that failed. The DTAP gate + routing rules re-apply
                    // inside the dispatcher.
                    let fetched = consumer
                        .fetch_message(&retry_state.stream_key, &retry_state.stream_entry_id)
                        .await;

                    match fetched {
                        Ok(Some(message)) => {
                            match dispatcher
                                .dispatch_channels(&message, &retry_state.failed_channels)
                                .await
                            {
                                Ok(result) if result.failed_channels.is_empty() => {
                                    retry_manager
                                        .clear_retry_state(&retry_state.site_id, &retry_state.message_id)
                                        .await
                                        .ok();
                                    if let Some(ref store) = message_store {
                                        store
                                            .update_status(
                                                &retry_state.site_id,
                                                &retry_state.message_id,
                                                MessageStatus::Sent,
                                            )
                                            .ok();
                                    }
                                    writeback_status(&mut status_conn, &retry_state.site_id, &retry_state.stream_entry_id, "sent", retry_state.attempts as i64, None).await;
                                    info!(
                                        message_id = %retry_state.message_id,
                                        "Retry succeeded"
                                    );
                                }
                                Ok(result) => {
                                    // Still failing: re-record with backoff;
                                    // RetryManager gives up at max_attempts.
                                    let failed: Vec<String> = result
                                        .failed_channels
                                        .iter()
                                        .map(|e| e.channel.clone())
                                        .collect();
                                    let err = result
                                        .failed_channels
                                        .first()
                                        .map(|e| e.error.to_string())
                                        .unwrap_or_default();
                                    writeback_status(&mut status_conn, &retry_state.site_id, &retry_state.stream_entry_id, "failed", (retry_state.attempts + 1) as i64, Some(&err)).await;
                                    retry_manager
                                        .record_failure(
                                            &retry_state.message_id,
                                            &retry_state.site_id,
                                            &retry_state.stream_key,
                                            &retry_state.stream_entry_id,
                                            &err,
                                            failed,
                                            None,
                                        )
                                        .await
                                        .ok();
                                }
                                Err(e) => {
                                    warn!(
                                        message_id = %retry_state.message_id,
                                        error = %e,
                                        "Retry dispatch errored; re-recording with backoff"
                                    );
                                    retry_manager
                                        .record_failure(
                                            &retry_state.message_id,
                                            &retry_state.site_id,
                                            &retry_state.stream_key,
                                            &retry_state.stream_entry_id,
                                            &e.to_string(),
                                            retry_state.failed_channels.clone(),
                                            None,
                                        )
                                        .await
                                        .ok();
                                }
                            }
                        }
                        Ok(None) => {
                            warn!(
                                message_id = %retry_state.message_id,
                                stream = %retry_state.stream_key,
                                "Message trimmed from stream history; abandoning retry"
                            );
                            retry_manager
                                .clear_retry_state(&retry_state.site_id, &retry_state.message_id)
                                .await
                                .ok();
                        }
                        Err(e) => {
                            // Transient read failure — keep the state, the
                            // next interval will try again.
                            warn!(
                                message_id = %retry_state.message_id,
                                error = %e,
                                "Could not re-fetch message for retry"
                            );
                        }
                    }
                }
            }
        }

        // Refresh the liveness heartbeat (~60s cadence, 120s TTL).
        if last_heartbeat.elapsed() >= Duration::from_secs(60) {
            last_heartbeat = Instant::now();
            write_heartbeat(&mut heartbeat_conn, &cli.environment).await;
        }

        // ── Inbound message processing ─────────────────────────────────────
        // On each iteration, first drain any pending entries from previous runs
        // (daemon may have crashed mid-processing, leaving entries delivered but
        // un-ACKed). Then read new entries with ">". This prevents messages from
        // getting permanently stuck in the pending queue after a crash.
        //
        // Uses redis::Value directly + manual parsing because redis crate has no
        // built-in FromRedisValue for the nested XREADGROUP response shape.
        {
            // Pass 1: drain pending entries (ID "0" = our own pending, oldest first)
            let pending_raw: redis::RedisResult<redis::Value> =
                redis::cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg("geodineum_comms_inbound")
                    .arg("comms_processor")
                    .arg("COUNT")
                    .arg(10)
                    .arg("STREAMS")
                    .arg(&inbound_stream_key)
                    .arg("0")
                    .query_async(&mut inbound_conn)
                    .await;

            // Pass 2: fetch new entries
            let new_raw: redis::RedisResult<redis::Value> =
                redis::cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg("geodineum_comms_inbound")
                    .arg("comms_processor")
                    .arg("COUNT")
                    .arg(10)
                    .arg("BLOCK")
                    .arg(100_u64)
                    .arg("STREAMS")
                    .arg(&inbound_stream_key)
                    .arg(">")
                    .query_async(&mut inbound_conn)
                    .await;

            // Log any errors (instead of silently swallowing)
            if let Err(ref e) = pending_raw {
                tracing::debug!(error = %e, "Inbound pending XREADGROUP error");
            }
            if let Err(ref e) = new_raw {
                tracing::debug!(error = %e, "Inbound new XREADGROUP error");
            }

            // Parse both results into (stream_name, entries) pairs
            let mut merged: Vec<(String, Vec<(String, HashMap<String, String>)>)> = Vec::new();
            if let Ok(v) = pending_raw {
                if let Some(parsed) = parse_xreadgroup_response(&v) {
                    merged.extend(parsed);
                }
            }
            if let Ok(v) = new_raw {
                if let Some(parsed) = parse_xreadgroup_response(&v) {
                    for (stream_name, entries) in parsed {
                        if let Some(existing) = merged.iter_mut().find(|(s, _)| s == &stream_name) {
                            existing.1.extend(entries);
                        } else {
                            merged.push((stream_name, entries));
                        }
                    }
                }
            }

            let inbound_result: Option<Vec<(String, Vec<(String, HashMap<String, String>)>)>> =
                if merged.is_empty() { None } else { Some(merged) };

            if let Some(streams) = inbound_result {
                let total_entries: usize = streams.iter().map(|(_, e)| e.len()).sum();
                if total_entries > 0 {
                    info!(count = total_entries, "Inbound: processing entries");
                }
                for (_stream_name, entries) in streams {
                    for (entry_id, fields) in entries {
                        let text = fields.get("text").cloned().unwrap_or_default();
                        let chat_id = fields.get("chat_id").cloned().unwrap_or_default();
                        let operator_id = fields.get("operator_id").cloned().unwrap_or_default();
                        let operator_name = fields.get("operator_name").cloned().unwrap_or_default();

                        info!(
                            entry_id = %entry_id,
                            chat_id = %chat_id,
                            text_len = text.len(),
                            "Inbound: parsed entry"
                        );

                        if text.is_empty() || chat_id.is_empty() {
                            // ACK and skip malformed entries
                            let _: redis::RedisResult<()> = redis::cmd("XACK")
                                .arg(&inbound_stream_key)
                                .arg("geodineum_comms_inbound")
                                .arg(&entry_id)
                                .query_async(&mut inbound_conn)
                                .await;
                            continue;
                        }

                        // Get or create conversation state
                        let state = conv_state
                            .get_or_create(&chat_id, &inbound_site_id, &operator_id, &operator_name)
                            .await;

                        let (pipeline, session_id) = match &state {
                            Ok(s) => (s.pipeline.clone(), s.session_id.clone()),
                            Err(_) => (default_pipeline.clone(), String::new()),
                        };

                        // Build InboundMessage for routing
                        let inbound_msg = geodineum_comms::inbound::telegram_receiver::InboundMessage {
                            text: text.clone(),
                            chat_id: chat_id.clone(),
                            operator_id: operator_id.clone(),
                            operator_name: operator_name.clone(),
                            channel_source: fields.get("channel_source").cloned().unwrap_or_else(|| "telegram".into()),
                            reply_to_msg_id: fields.get("reply_to_msg_id").and_then(|s| s.parse().ok()),
                            timestamp: fields.get("ts").cloned().unwrap_or_default(),
                            chat_type: fields.get("chat_type").cloned().unwrap_or_else(|| "private".into()),
                            is_callback: fields.get("is_callback").map(|s| s == "true").unwrap_or(false),
                            callback_query_id: fields.get("callback_query_id").cloned().unwrap_or_default(),
                            callback_message_id: fields.get("callback_message_id").and_then(|s| s.parse().ok()),
                        };

                        // Route the command
                        match command_router
                            .route_command(&inbound_msg, &mut conv_state, &inbound_site_id, &pipeline, &session_id)
                            .await
                        {
                            Ok(action) => {
                                info!(
                                    chat_id = %chat_id,
                                    action = ?std::mem::discriminant(&action),
                                    "Inbound: routed to action"
                                );
                                match action {
                                    CommandAction::Local { command, params } => {
                                        // an earlier hardening pass: command text comes from Telegram operator input;
                                        // normalize before tracing to prevent log-line forgery.
                                        info!(command = %geodineum_comms::inbound::log_safe(&command), "Handling local command");
                                        let response_text = handle_local_command(&command, &params);
                                        // Reply directly via Telegram (skip outbound pipeline)
                                        if let Some(ref token) = inbound_bot_token {
                                            let _ = send_telegram_reply(token, &chat_id, &response_text).await;
                                        }
                                    }
                                    CommandAction::Inference { prompt, pipeline, session_id } => {
                                        info!(
                                            pipeline = %pipeline,
                                            chat_id = %chat_id,
                                            "Routing to inference service"
                                        );

                                        let unified_stream = format!(
                                            "{{{}}}:gnode:unified:{}",
                                            inbound_site_id, cli.environment
                                        );

                                        // Send processing indicator + spawn response poller
                                        // (only if we have a bot token for Telegram replies)
                                        let processing_msg_id = if let Some(ref token) = inbound_bot_token {
                                            match send_processing_indicator(token, &chat_id, &pipeline).await {
                                                Ok(id) => id,
                                                Err(e) => {
                                                    warn!(error = %e, "Failed to send processing indicator");
                                                    0
                                                }
                                            }
                                        } else {
                                            0
                                        };

                                        // XADD to the inference unified stream
                                        let request_id = format!("comms-{}-{}", chat_id, chrono::Utc::now().timestamp_millis());
                                        let params_json = serde_json::json!({
                                            "prompt": prompt,
                                            "pipeline": pipeline,
                                            "consumer": format!("comms:{}", operator_id),
                                            "session_id": session_id,
                                        });

                                        let _: redis::RedisResult<String> = redis::cmd("XADD")
                                            .arg(&unified_stream)
                                            .arg("*")
                                            .arg(&[
                                                ("id", request_id.as_str()),
                                                ("cmd", "direct"),
                                                ("params", &params_json.to_string()),
                                                ("_gh", "inference"),
                                            ])
                                            .query_async(&mut inbound_conn)
                                            .await;

                                        // Spawn async response poller (non-blocking)
                                        if let Some(ref token) = inbound_bot_token {
                                            if processing_msg_id > 0 {
                                                spawn_response_poller(
                                                    conn.clone(),
                                                    PollRequest {
                                                        bot_token: token.clone(),
                                                        chat_id: chat_id.clone(),
                                                        processing_msg_id,
                                                        unified_stream: unified_stream.clone(),
                                                        request_id: request_id.clone(),
                                                        pipeline: pipeline.clone(),
                                                        timeout_secs: inference_timeout,
                                                    },
                                                );
                                            }
                                        }

                                        // Record message in conversation state
                                        conv_state
                                            .record_message(&chat_id, &inbound_site_id, &text)
                                            .await
                                            .ok();
                                    }
                                    CommandAction::ComponentReply { resolution } => {
                                        info!(
                                            component = %resolution.component,
                                            command = %resolution.command,
                                            "Routing reply to component"
                                        );
                                        // XADD reply to the component's callback stream
                                        let _reply_json = serde_json::json!({
                                            "command": resolution.command,
                                            "context_id": resolution.context_id,
                                            "component": resolution.component,
                                            "operator_id": operator_id,
                                            "operator_name": operator_name,
                                            "channel_source": "telegram",
                                            "ts": chrono::Utc::now().to_rfc3339(),
                                        });

                                        let _: redis::RedisResult<String> = redis::cmd("XADD")
                                            .arg(&resolution.callback_stream)
                                            .arg("*")
                                            .arg(&[
                                                ("command", resolution.command.as_str()),
                                                ("context_id", resolution.context_id.as_str()),
                                                ("component", resolution.component.as_str()),
                                                ("operator_id", operator_id.as_str()),
                                                ("operator_name", operator_name.as_str()),
                                                ("channel_source", "telegram"),
                                                ("ts", &chrono::Utc::now().to_rfc3339()),
                                            ])
                                            .query_async(&mut inbound_conn)
                                            .await;
                                    }
                                    CommandAction::SetPipeline { pipeline } => {
                                        conv_state
                                            .set_pipeline(&chat_id, &inbound_site_id, &pipeline)
                                            .await
                                            .ok();
                                        if let Some(ref token) = inbound_bot_token {
                                            let _ = send_telegram_reply(
                                                token,
                                                &chat_id,
                                                &format!("Pipeline switched to <code>{}</code>", pipeline),
                                            ).await;
                                        }
                                    }
                                    CommandAction::Reset => {
                                        conv_state
                                            .reset(&chat_id, &inbound_site_id)
                                            .await
                                            .ok();
                                        if let Some(ref token) = inbound_bot_token {
                                            let _ = send_telegram_reply(
                                                token,
                                                &chat_id,
                                                "Conversation reset. Next message starts a fresh session.",
                                            ).await;
                                        }
                                    }
                                    CommandAction::Start => {
                                        if let Some(ref token) = inbound_bot_token {
                                            let keyboard = geodineum_comms::inbound::build_inline_keyboard(vec![
                                                vec![
                                                    geodineum_comms::inbound::InlineButton {
                                                        text: "About".into(),
                                                        callback_data: "about".into(),
                                                    },
                                                    geodineum_comms::inbound::InlineButton {
                                                        text: "Settings".into(),
                                                        callback_data: "settings".into(),
                                                    },
                                                ],
                                            ]);
                                            let welcome = format!(
                                                "Welcome, <b>{}</b>!\n\n\
                                                 Connected to the Geodineum constellation via the \
                                                 <code>{}</code> pipeline.\n\n\
                                                 Send a message to start a conversation.",
                                                operator_name, pipeline
                                            );
                                            let _ = send_telegram_message_with_keyboard(
                                                token, &chat_id, &welcome, &keyboard,
                                            ).await;
                                        }
                                    }
                                    CommandAction::Callback { data, query_id, message_id: _ } => {
                                        if let Some(ref token) = inbound_bot_token {
                                            // Answer the callback (removes loading spinner)
                                            answer_callback(token, &query_id, None).await;

                                            match data.as_str() {
                                                "about" => {
                                                    let text = format!(
                                                        "<b>Geodineum-COMMS Advisor</b>\n\
                                                         Active pipeline: <code>{}</code>\n\n\
                                                         Connected to the Geodineum constellation via ValKey streams.\n\
                                                         Inference backed by the configured inference service.",
                                                        pipeline
                                                    );
                                                    let _ = send_telegram_reply(token, &chat_id, &text).await;
                                                }
                                                "settings" => {
                                                    let kb = geodineum_comms::inbound::build_inline_keyboard(vec![
                                                        vec![
                                                            geodineum_comms::inbound::InlineButton {
                                                                text: "Switch Pipeline".into(),
                                                                callback_data: "switch_pipeline".into(),
                                                            },
                                                            geodineum_comms::inbound::InlineButton {
                                                                text: "System Status".into(),
                                                                callback_data: "system_status".into(),
                                                            },
                                                        ],
                                                    ]);
                                                    let _ = send_telegram_message_with_keyboard(
                                                        token, &chat_id, "Settings", &kb,
                                                    ).await;
                                                }
                                                "switch_pipeline" => {
                                                    // Live pipeline list from Geodine's published registry
                                                    // ({site}:gnode:pipeline:_index, a JSON array the pipeline
                                                    // runner refreshes at startup). The static list is only a
                                                    // fallback for when the registry is absent — hardcoding it
                                                    // hid every pipeline added after the list was written
                                                    // (aurelius, scn_transcoder, ...).
                                                    let index_key = format!("{{{}}}:gnode:pipeline:_index", inbound_site_id);
                                                    let registry: Vec<String> = redis::cmd("GET")
                                                        .arg(&index_key)
                                                        .query_async::<Option<String>>(&mut inbound_conn)
                                                        .await
                                                        .ok()
                                                        .flatten()
                                                        .and_then(|s| serde_json::from_str(&s).ok())
                                                        .unwrap_or_default();
                                                    let pipelines: Vec<String> = if registry.is_empty() {
                                                        ["sysadmin", "sales_chat", "code_security", "log_monitor"]
                                                            .iter().map(|s| s.to_string()).collect()
                                                    } else {
                                                        registry
                                                    };
                                                    let buttons: Vec<Vec<geodineum_comms::inbound::InlineButton>> = pipelines
                                                        .iter()
                                                        .map(|p| {
                                                            let label = if *p == pipeline {
                                                                format!("{} (active)", p)
                                                            } else {
                                                                p.to_string()
                                                            };
                                                            vec![geodineum_comms::inbound::InlineButton {
                                                                text: label,
                                                                callback_data: format!("pipeline_{}", p),
                                                            }]
                                                        })
                                                        .collect();
                                                    let kb = geodineum_comms::inbound::build_inline_keyboard(buttons);
                                                    let _ = send_telegram_message_with_keyboard(
                                                        token, &chat_id,
                                                        &format!("{} pipelines available.", pipelines.len()),
                                                        &kb,
                                                    ).await;
                                                }
                                                "system_status" => {
                                                    // Read pipeline metrics from ValKey
                                                    let status_text = get_pipeline_status(&mut inbound_conn, &inbound_site_id).await;
                                                    let _ = send_telegram_reply(token, &chat_id, &status_text).await;
                                                }
                                                d if d.starts_with("pipeline_") => {
                                                    let name = &d["pipeline_".len()..];
                                                    conv_state
                                                        .set_pipeline(&chat_id, &inbound_site_id, name)
                                                        .await
                                                        .ok();
                                                    answer_callback(token, &query_id, Some(&format!("Switched to {}", name))).await;
                                                }
                                                _ => {
                                                    debug!(callback_data = %data, "Unknown callback");
                                                }
                                            }
                                        }
                                    }
                                    CommandAction::History => {
                                        if let Some(ref token) = inbound_bot_token {
                                            // Read last messages from the inference service conversation history
                                            let history = get_conversation_history(
                                                &mut inbound_conn, &inbound_site_id, &session_id,
                                            ).await;
                                            let _ = send_telegram_reply(token, &chat_id, &history).await;
                                        }
                                    }
                                    CommandAction::ChainInference { prompt, session_id } => {
                                        info!(chat_id = %chat_id, "Routing to staged inference chain");

                                        let unified_stream = format!(
                                            "{{{}}}:gnode:unified:{}",
                                            inbound_site_id, cli.environment
                                        );

                                        // Send processing indicator
                                        let processing_msg_id = if let Some(ref token) = inbound_bot_token {
                                            match send_processing_indicator(token, &chat_id, "staged inference (draft→review→guard)").await {
                                                Ok(id) => id,
                                                Err(_) => 0,
                                            }
                                        } else {
                                            0
                                        };

                                        // Run the chain (blocking — sequential 3-stage pipeline)
                                        let chain_result = geodineum_comms::inbound::run_inference_chain(
                                            &mut inbound_conn,
                                            &unified_stream,
                                            &prompt,
                                            &operator_id,
                                            &session_id,
                                            &chain_config,
                                        )
                                        .await;

                                        // Deliver result
                                        if let Some(ref token) = inbound_bot_token {
                                            // an earlier hardening pass: AI-generated text must be HTML-escaped
                                            // before embedding into the parse_mode=HTML payload.
                                            let response_text = match chain_result {
                                                Ok(result) => {
                                                    let safe = geodineum_comms::inbound::html_escape_telegram(&result.text);
                                                    if result.filtered {
                                                        safe
                                                    } else {
                                                        format!("{}\n\n<i>staged inference | 3-stage chain</i>", safe)
                                                    }
                                                }
                                                Err(e) => format!(
                                                    "Inference chain error: {}",
                                                    geodineum_comms::inbound::html_escape_telegram(&e.to_string())
                                                ),
                                            };

                                            if processing_msg_id > 0 {
                                                let client = reqwest::Client::new();
                                                let url = format!("https://api.telegram.org/bot{}/editMessageText", token);
                                                let payload = serde_json::json!({
                                                    "chat_id": chat_id,
                                                    "message_id": processing_msg_id,
                                                    "text": response_text,
                                                    "parse_mode": "HTML",
                                                });
                                                let _ = client.post(&url).json(&payload).send().await;
                                            } else {
                                                let _ = send_telegram_reply(token, &chat_id, &response_text).await;
                                            }
                                        }

                                        conv_state
                                            .record_message(&chat_id, &inbound_site_id, &text)
                                            .await
                                            .ok();
                                    }
                                    CommandAction::WorkflowDispatch { intent } => {
                                        info!(
                                            workflow = %intent.workflow_id,
                                            "Dispatching workflow to the workflow-engine stream"
                                        );

                                        let wf_dispatcher = WorkflowDispatcher::new(
                                            &inbound_site_id,
                                            &cli.environment,
                                        );

                                        match wf_dispatcher.dispatch(
                                            &mut inbound_conn,
                                            &intent,
                                            &operator_id,
                                            &operator_name,
                                        ).await {
                                            Ok(execution_id) => {
                                                let confirmation = WorkflowDispatcher::format_dispatch_confirmation(
                                                    &intent, &execution_id,
                                                );
                                                if let Some(ref token) = inbound_bot_token {
                                                    let _ = send_telegram_reply(token, &chat_id, &confirmation).await;
                                                }
                                            }
                                            Err(e) => {
                                                if let Some(ref token) = inbound_bot_token {
                                                    let _ = send_telegram_reply(
                                                        token, &chat_id,
                                                        &format!("Workflow dispatch failed: {}", e),
                                                    ).await;
                                                }
                                            }
                                        }
                                    }
                                    CommandAction::NoOp { reason } => {
                                        tracing::debug!(reason = %reason, "No-op command");
                                    }
                                }
                            }
                            Err(e) => {
                                error!(error = %e, chat_id = %chat_id, "Failed to route inbound command");
                            }
                        }

                        // ACK the inbound stream entry
                        let _: redis::RedisResult<()> = redis::cmd("XACK")
                            .arg(&inbound_stream_key)
                            .arg("geodineum_comms_inbound")
                            .arg(&entry_id)
                            .query_async(&mut inbound_conn)
                            .await;
                    }
                }
            }
        }

        // Log backoff stats periodically (every ~100 iterations)
        static ITER_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let count = ITER_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count % 100 == 0 {
            let stats = backoff_tracker.stats();
            if stats.total_failures > 0 || rate_limiter.total_limited > 0 {
                info!(
                    healthy = stats.healthy,
                    in_backoff = stats.in_backoff,
                    unhealthy = stats.unhealthy,
                    total_failures = stats.total_failures,
                    rate_limited = rate_limiter.total_limited,
                    "Backoff and rate limit stats"
                );
            }
        }

        // Periodic cleanup based on retention config
        if config.retention.enabled {
            static LAST_CLEANUP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let last = LAST_CLEANUP.load(std::sync::atomic::Ordering::Relaxed);

            if now_secs - last >= config.retention.cleanup_interval_secs {
                LAST_CLEANUP.store(now_secs, std::sync::atomic::Ordering::Relaxed);

                if let Some(ref store) = message_store {
                    match store.cleanup_all(&config.retention) {
                        Ok(results) => {
                            let total_deleted: u64 = results.iter().map(|(_, r)| r.total_deleted).sum();
                            if total_deleted > 0 {
                                info!(
                                    sites = results.len(),
                                    total_deleted = total_deleted,
                                    "Periodic cleanup completed"
                                );
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "Periodic cleanup failed");
                        }
                    }
                }
            }
        }
    }
}

/// Write the real dispatch status back to ValKey so the wp-admin dashboard
/// reflects delivery instead of the stream's frozen "pending". The daemon only
/// records status in SQLite + XACKs the stream; the PHP dashboard reads
/// `dispatch.status` off the (immutable) stream entry, so it always showed
/// "pending". This mirrors the terminal status into a per-message hash the
/// dashboard overlays, keyed by the stream entry id.
///
/// Key: {site}:gnode:comms:status:<stream_id>  — under {site}:gnode:* so the
/// per-site dashboard ACL can read it. Best-effort; a failure here never blocks
/// dispatch (SQLite remains the source of truth).
async fn writeback_status(
    conn: &mut redis::aio::MultiplexedConnection,
    site_id: &str,
    stream_id: &str,
    status: &str,
    attempts: i64,
    error: Option<&str>,
) {
    let key = format!("{{{}}}:gnode:comms:status:{}", site_id, stream_id);
    let now = chrono::Utc::now().to_rfc3339();
    let res: redis::RedisResult<()> = redis::pipe()
        .cmd("HSET")
        .arg(&key)
        .arg("status").arg(status)
        .arg("attempts").arg(attempts)
        .arg("last_attempt").arg(&now)
        .arg("error").arg(error.unwrap_or(""))
        .ignore()
        .cmd("EXPIRE").arg(&key).arg(2_592_000i64).ignore() // 30 days
        .query_async(conn)
        .await;
    if let Err(e) = res {
        debug!(site_id = %site_id, stream_id = %stream_id, error = %e, "comms status writeback failed (non-fatal)");
    }

    // T-E: the durable half. The hash above is a mutable, unsigned projection
    // the dashboard overlays; this is the tamper-evident record observers
    // consume. ADDITIVE for now — the hash stays until the dashboard read-path
    // moves, per the contract's emit-then-remove discipline, which is the same
    // rule that gated T-D on the daemon side.
    //
    // Best-effort like the writeback itself: SQLite remains the source of
    // truth, and a receipt failure must never block or fail a dispatch.
    if let Some(ctx) = geodineum_comms::receipt::receipt_context() {
        let now = geodineum_comms::receipt::now_ms();
        if let Some(receipt) = geodineum_comms::receipt::signed_delivery_receipt(
            stream_id,
            "comms.deliver",
            status,
            error.map(String::from),
            site_id,
            &key,
            status,
            now,
        ) {
            if let Err(e) = geodineum_comms::receipt::emit_receipt(
                conn, &receipt, site_id, &ctx.environment, now,
            )
            .await
            {
                debug!(site_id = %site_id, stream_id = %stream_id, error = %e,
                       "comms receipt emit failed (non-fatal)");
            }
        }
    }
}

/// Best-effort liveness heartbeat so the operator dashboard can show COMMS as
/// up. SETEX with a 120s TTL, refreshed ~every 60s from the main loop; a dead
/// daemon's key self-expires and the dashboard reads it as down. Keyed under
/// {geodineum}:gnode:* so every service ACL already grants the write.
async fn write_heartbeat(conn: &mut redis::aio::MultiplexedConnection, environment: &str) {
    let ns = std::env::var("GNODE_TOPOLOGY_NAMESPACE").unwrap_or_else(|_| "geodineum".to_string());
    // Node segment per CONTRACTS/heartbeat.md: without it, every node in a
    // constellation wrote the same key and last-writer-won — the dashboard
    // could not say WHICH node COMMS ran on, and a dead instance hid behind
    // a live one's fresh ts. First dot-label of the hostname, matching the
    // daemon's GNODE_NODE_ID convention (short hostname).
    let node = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .and_then(|h| h.split('.').next().map(str::to_string))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown-node".to_string());
    let key = format!("{{{}}}:gnode:heartbeat:{}:comms:{}", ns, environment, node);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let value = format!(
        "{{\"ts\":{},\"pid\":{},\"comp\":\"comms\",\"node\":\"{}\"}}",
        ts, std::process::id(), node);
    let res: redis::RedisResult<()> = redis::cmd("SETEX")
        .arg(&key)
        .arg(120)
        .arg(value)
        .query_async(conn)
        .await;
    if let Err(e) = res {
        debug!(error = %e, "comms heartbeat failed (non-fatal)");
    }
}

/// Archive a message to SQLite and update its status
fn archive_message(
    store: &MessageStore,
    message: &CommsMessage,
    stream_id: &str,
    status: MessageStatus,
) -> anyhow::Result<()> {
    // Archive the message (creates record with 'pending' status)
    store.archive_received(&message.site_id, message, stream_id)?;

    // Update status if not pending
    if status != MessageStatus::Pending {
        store.update_status(&message.site_id, &message.id, status)?;
    }

    Ok(())
}

/// Parse a raw XREADGROUP/XREAD response into (stream_name, [(entry_id, fields)]).
///
/// The redis crate doesn't have a built-in FromRedisValue for this nested shape,
/// so we walk the redis::Value tree manually. Returns None for nil or unparseable
/// responses.
///
/// Expected shape:
///   Bulk([
///     Bulk([ stream_name, Bulk([
///       Bulk([ entry_id, Bulk([field, value, field, value, ...]) ]),
///       ...
///     ]) ]),
///     ...
///   ])
fn parse_xreadgroup_response(
    value: &redis::Value,
) -> Option<Vec<(String, Vec<(String, HashMap<String, String>)>)>> {
    let streams = match value {
        redis::Value::Array(s) => s,
        redis::Value::Nil => return None,
        _ => return None,
    };

    let mut out = Vec::new();
    for stream_entry in streams {
        let stream_parts = match stream_entry {
            redis::Value::Array(p) => p,
            _ => continue,
        };
        if stream_parts.len() < 2 {
            continue;
        }

        let stream_name: String = match redis::from_redis_value(&stream_parts[0]) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let entries_value = &stream_parts[1];
        let entries_list = match entries_value {
            redis::Value::Array(e) => e,
            _ => continue,
        };

        let mut entries_out = Vec::new();
        for entry in entries_list {
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

            // Fields: flat list [field, value, field, value, ...]
            let field_values: Vec<String> = match redis::from_redis_value(&entry_parts[1]) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let mut fields = HashMap::new();
            let mut iter = field_values.into_iter();
            while let (Some(k), Some(v)) = (iter.next(), iter.next()) {
                fields.insert(k, v);
            }

            entries_out.push((entry_id, fields));
        }

        out.push((stream_name, entries_out));
    }

    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Send a text reply directly to a Telegram chat (bypasses outbound pipeline)
async fn send_telegram_reply(bot_token: &str, chat_id: &str, text: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token);
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
    });
    client.post(&url).json(&payload).send().await?;
    Ok(())
}

/// Send a Telegram message with inline keyboard
async fn send_telegram_message_with_keyboard(
    bot_token: &str,
    chat_id: &str,
    text: &str,
    reply_markup: &serde_json::Value,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token);
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
        "reply_markup": reply_markup,
    });
    client.post(&url).json(&payload).send().await?;
    Ok(())
}

/// Answer a Telegram callback query (removes loading spinner from button)
async fn answer_callback(bot_token: &str, query_id: &str, text: Option<&str>) {
    let client = reqwest::Client::new();
    let url = format!(
        "https://api.telegram.org/bot{}/answerCallbackQuery",
        bot_token
    );
    let mut payload = serde_json::json!({ "callback_query_id": query_id });
    if let Some(t) = text {
        payload["text"] = serde_json::Value::String(t.to_string());
    }
    let _ = client.post(&url).json(&payload).send().await;
}

/// Read pipeline status from ValKey metrics
async fn get_pipeline_status(
    conn: &mut redis::aio::MultiplexedConnection,
    site_id: &str,
) -> String {
    let pattern = format!("{{{}}}:inference:metrics:*", site_id);
    // an earlier hardening pass: SCAN cursor instead of blocking KEYS.
    let keys: Vec<String> = geodineum_comms::valkey_scan::scan_keys(conn, &pattern)
        .await
        .unwrap_or_default();

    if keys.is_empty() {
        return "Could not reach the inference service or no pipelines are running.".to_string();
    }

    let mut lines = vec!["<b>Inference Pipeline Status</b>\n".to_string()];
    for key in &keys {
        if key.ends_with(":_aggregate") {
            continue;
        }
        let name = key.rsplit(':').next().unwrap_or("?");
        let data: std::collections::HashMap<String, String> = redis::cmd("HGETALL")
            .arg(key)
            .query_async(conn)
            .await
            .unwrap_or_default();
        let model = data.get("model").cloned().unwrap_or_else(|| "?".into());
        let running = data.get("running").map(|s| s == "1").unwrap_or(false);
        if running {
            lines.push(format!("  <code>{}</code> — {}", name, model));
        }
    }

    if lines.len() == 1 {
        return "No running pipelines detected.".to_string();
    }

    lines.join("\n")
}

/// Read conversation history from the inference service ConversationStore in ValKey
async fn get_conversation_history(
    conn: &mut redis::aio::MultiplexedConnection,
    site_id: &str,
    session_id: &str,
) -> String {
    if session_id.is_empty() {
        return "No active session. Send a message to start a conversation.".to_string();
    }

    // The inference service stores history at {site_id}:inference:history:{session_id}
    // (brace-literal hash-tag form for cluster-mode co-location).
    let key = format!("{{{}}}:inference:history:{}", site_id, session_id);
    let entries: Vec<String> = redis::cmd("LRANGE")
        .arg(&key)
        .arg(0)
        .arg(19) // Last 20 entries
        .query_async(conn)
        .await
        .unwrap_or_default();

    if entries.is_empty() {
        return "No conversation history found for this session.".to_string();
    }

    let mut lines = vec!["<b>Conversation History</b>\n".to_string()];
    for entry in &entries {
        // Each entry is JSON: {"role": "user"|"assistant", "content": "..."}
        if let Ok(msg) = serde_json::from_str::<serde_json::Value>(entry) {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("?");
            let content = msg
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let prefix = if role == "user" { "You" } else { "Assistant" };
            // Truncate long messages
            let short = if content.len() > 200 {
                format!("{}...", &content[..200])
            } else {
                content.to_string()
            };
            lines.push(format!("<b>{}</b>: {}", prefix, short));
        }
    }

    lines.join("\n\n")
}

/// Handle a local command (executed within COMMS, not routed to components)
fn handle_local_command(command: &str, params: &[String]) -> String {
    match command {
        "status" | "health" => "Geodineum-COMMS is running. Use /pipeline to see active pipeline.".to_string(),
        "help" => {
            "/status — System health\n\
             /pipeline [name] — View or switch pipeline\n\
             /reset — Reset conversation\n\
             /help — This message\n\n\
             Reply to alerts with QUARANTINE, DISMISS, RETRY, or UPDATE."
                .to_string()
        }
        "pipeline_info" => {
            let current = params.first().map(|s| s.as_str()).unwrap_or("unknown");
            format!("Current pipeline: {}", current)
        }
        "stats" => "Detailed statistics: wp-admin \u{2192} Geodineum \u{2192} Comms on any constellation site.".to_string(),
        "sites" => "Site listing: `geodineum status` on the node, or wp-admin \u{2192} Geodineum \u{2192} Comms.".to_string(),
        _ => format!("Unknown command: {}", command),
    }
}

/// Write a response message to the comms outbound stream for delivery back to the operator
async fn run_test(
    cli: &Cli,
    _config: &Config,
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

/// Get data directory from environment or default
fn get_data_dir() -> String {
    std::env::var("GEODINEUM_COMMS_DATA_DIR").unwrap_or_else(|_| "/var/lib/geodineum-comms".to_string())
}

/// List all sites
fn run_sites_command() -> anyhow::Result<()> {
    let store = MessageStore::new(&get_data_dir())?;
    let sites = cli::list_sites(&store)?;
    TablePrinter::print_sites(&sites, OutputFormat::Table)?;
    Ok(())
}

/// List messages
fn run_messages_command(
    site_id: Option<&str>,
    status: Option<&str>,
    message_type: Option<&str>,
    limit: usize,
    format: &str,
    verbose: bool,
) -> anyhow::Result<()> {
    let store = MessageStore::new(&get_data_dir())?;
    let messages = cli::list_messages(&store, site_id, status, message_type, limit)?;
    TablePrinter::print_messages(&messages, OutputFormat::from_str(format), verbose)?;
    Ok(())
}

/// Show stats
fn run_stats_command(site_id: Option<&str>, format: &str) -> anyhow::Result<()> {
    let store = MessageStore::new(&get_data_dir())?;
    let stats = cli::get_stats(&store, site_id)?;
    TablePrinter::print_stats(&stats, OutputFormat::from_str(format))?;
    Ok(())
}

/// Show message detail
fn run_message_command(site_id: &str, message_id: &str, format: &str) -> anyhow::Result<()> {
    let store = MessageStore::new(&get_data_dir())?;
    match cli::get_message(&store, site_id, message_id)? {
        Some(msg) => {
            TablePrinter::print_message_detail(&msg, OutputFormat::from_str(format))?;
        }
        None => {
            println!("Message not found: {} in site {}", message_id, site_id);
        }
    }
    Ok(())
}

/// Retry a failed message
fn run_retry_command(site_id: &str, message_id: &str) -> anyhow::Result<()> {
    let store = MessageStore::new(&get_data_dir())?;
    match cli::retry_message(&store, site_id, message_id)? {
        true => {
            println!("Message {} queued for retry", message_id);
        }
        false => {
            println!(
                "Cannot retry message {} - not found or not in a retryable state",
                message_id
            );
        }
    }
    Ok(())
}

/// Run cleanup based on retention policies
fn run_cleanup_command(
    site_id: Option<&str>,
    max_age_days: Option<u32>,
    delete_spam: bool,
    vacuum: bool,
    dry_run: bool,
    config: &Config,
) -> anyhow::Result<()> {
    let store = MessageStore::new(&get_data_dir())?;

    // Build retention config with overrides
    let mut retention = config.retention.clone();

    if let Some(days) = max_age_days {
        retention.max_age_days = days;
    }

    if delete_spam {
        retention.spam_policy = SpamRetentionPolicy::DeleteImmediately;
    }

    retention.vacuum_after_cleanup = vacuum;

    if dry_run {
        println!("\n{:=^70}", " DRY RUN - No changes will be made ");
        println!("\nRetention policy:");
        println!("  Max age: {} days", retention.max_age_days);
        println!("  Max messages per site: {}", if retention.max_messages_per_site == 0 { "unlimited".to_string() } else { retention.max_messages_per_site.to_string() });
        println!("  Max DB size: {}", if retention.max_db_size_mb == 0 { "unlimited".to_string() } else { format!("{} MB", retention.max_db_size_mb) });
        println!("  Spam policy: {:?}", retention.spam_policy);
        println!("  Vacuum: {}", retention.vacuum_after_cleanup);
        println!();

        let sites = if let Some(id) = site_id {
            vec![id.to_string()]
        } else {
            store.list_sites()?
        };

        for site in &sites {
            match store.get_db_stats(site) {
                Ok(stats) => {
                    println!("Site: {}", site);
                    println!("  Messages: {}", stats.message_count);
                    println!("  Spam: {}", stats.spam_count);
                    println!("  Size: {}", stats.file_size_human());
                    if let Some(ref oldest) = stats.oldest_message {
                        println!("  Oldest: {}", &oldest[..19.min(oldest.len())]);
                    }
                    println!();
                }
                Err(e) => {
                    println!("Site: {} - Error: {}", site, e);
                }
            }
        }

        println!("{:=^70}", "");
        return Ok(());
    }

    // Run actual cleanup
    let start = Instant::now();

    let results = if let Some(id) = site_id {
        match store.cleanup_site(id, &retention) {
            Ok(result) => vec![(id.to_string(), result)],
            Err(e) => {
                println!("Cleanup failed for {}: {}", id, e);
                return Err(e.into());
            }
        }
    } else {
        store.cleanup_all(&retention)?
    };

    // Print results
    println!("\n{:=^70}", " Cleanup Results ");
    println!();

    let mut total_deleted = 0u64;
    let mut total_space = 0u64;

    for (site, result) in &results {
        if result.total_deleted > 0 || !result.errors.is_empty() {
            println!("Site: {}", site);
            println!("  Deleted by age:    {:>8}", result.deleted_by_age);
            println!("  Deleted spam:      {:>8}", result.deleted_spam);
            println!("  Deleted by count:  {:>8}", result.deleted_by_count);
            println!("  Deleted by size:   {:>8}", result.deleted_by_size);
            println!("  Total deleted:     {:>8}", result.total_deleted);
            if result.vacuumed {
                let reclaimed = result.space_reclaimed();
                println!("  Space reclaimed:   {:>8}", format_bytes(reclaimed));
                total_space += reclaimed;
            }
            for err in &result.errors {
                println!("  Warning: {}", err);
            }
            println!();
        }
        total_deleted += result.total_deleted;
    }

    println!("{:-^70}", "");
    println!("Total sites processed: {}", results.len());
    println!("Total messages deleted: {}", total_deleted);
    println!("Total space reclaimed: {}", format_bytes(total_space));
    println!("Elapsed time: {:?}", start.elapsed());
    println!("{:=^70}", "");

    Ok(())
}

/// Show database statistics
fn run_db_stats_command(site_id: Option<&str>, format: &str) -> anyhow::Result<()> {
    let store = MessageStore::new(&get_data_dir())?;

    let sites = if let Some(id) = site_id {
        vec![id.to_string()]
    } else {
        store.list_sites()?
    };

    if format == "json" {
        let mut stats_list = Vec::new();
        for site in &sites {
            match store.get_db_stats(site) {
                Ok(stats) => {
                    stats_list.push(serde_json::json!({
                        "site_id": stats.site_id,
                        "file_size_bytes": stats.file_size_bytes,
                        "file_size_human": stats.file_size_human(),
                        "message_count": stats.message_count,
                        "spam_count": stats.spam_count,
                        "oldest_message": stats.oldest_message,
                        "newest_message": stats.newest_message,
                    }));
                }
                Err(e) => {
                    stats_list.push(serde_json::json!({
                        "site_id": site,
                        "error": e.to_string(),
                    }));
                }
            }
        }
        println!("{}", serde_json::to_string_pretty(&stats_list)?);
    } else {
        println!("\n{:=^80}", " Database Statistics ");
        println!();
        println!(
            "  {:<25} {:>10} {:>10} {:>10} {:>15}",
            "SITE", "MESSAGES", "SPAM", "SIZE", "OLDEST"
        );
        println!("  {}", "-".repeat(75));

        let mut total_messages = 0u64;
        let mut total_spam = 0u64;
        let mut total_size = 0u64;

        for site in &sites {
            match store.get_db_stats(site) {
                Ok(stats) => {
                    let short_site = if site.len() > 23 {
                        format!("{}...", &site[..23])
                    } else {
                        site.clone()
                    };

                    let oldest = stats.oldest_message
                        .as_ref()
                        .map(|s| &s[..10.min(s.len())])
                        .unwrap_or("-");

                    println!(
                        "  {:<25} {:>10} {:>10} {:>10} {:>15}",
                        short_site,
                        stats.message_count,
                        stats.spam_count,
                        stats.file_size_human(),
                        oldest,
                    );

                    total_messages += stats.message_count;
                    total_spam += stats.spam_count;
                    total_size += stats.file_size_bytes;
                }
                Err(e) => {
                    println!("  {:<25} Error: {}", site, e);
                }
            }
        }

        println!("  {}", "-".repeat(75));
        println!(
            "  {:<25} {:>10} {:>10} {:>10}",
            "TOTAL",
            total_messages,
            total_spam,
            format_bytes(total_size),
        );
        println!();
        println!("{:=^80}", "");
    }

    Ok(())
}

/// Format bytes in human-readable form
fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
