//! Configuration management

use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Geodineum-COMMS Notification Daemon
#[derive(Parser, Debug)]
#[command(name = "geodineum-comms")]
#[command(about = "Notification daemon for gNode - dispatches notifications via email, Telegram, and SMS")]
#[command(version)]
pub struct Cli {
    /// ValKey host
    #[arg(long, default_value = "127.0.0.1", env = "VALKEY_HOST")]
    pub redis_host: String,

    /// ValKey port
    #[arg(long, default_value = "47445", env = "VALKEY_PORT")]
    pub redis_port: u16,

    /// ValKey ACL username
    #[arg(long, default_value = "geodineum_comms", env = "VALKEY_USER")]
    pub redis_user: String,

    /// ValKey ACL password
    #[arg(long, env = "VALKEY_AUTH")]
    pub redis_auth: Option<String>,

    /// Path to configuration file
    #[arg(long, short, default_value = "config/default.yaml")]
    pub config: PathBuf,

    /// Log level
    #[arg(long, default_value = "info", env = "LOG_LEVEL")]
    pub log_level: String,


    /// Unique node identifier
    #[arg(long, env = "NODE_ID")]
    pub node_id: Option<String>,

    /// DTAP environment filter
    #[arg(long, default_value = "production", env = "ENVIRONMENT")]
    pub environment: String,

    /// Allow REAL sends for non-production messages. Off by default: a message
    /// whose resolved DTAP environment is not "production" is dry-run (logged,
    /// not sent). Set only on a deliberately live-sending non-prod daemon.
    #[arg(long, default_value = "false", env = "ALLOW_NONPROD_SEND")]
    pub allow_nonprod_send: bool,

    /// Template directory
    #[arg(long, default_value = "config/templates")]
    pub template_dir: String,

    /// Subcommand
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Start the daemon
    Start,

    /// Stop the daemon (sends SIGTERM)
    Stop,

    /// Check daemon status
    Status,

    /// Test notification channels
    Test {
        /// Site ID to test
        #[arg(long)]
        site_id: String,

        /// Channel to test (email, telegram, sms, or all)
        #[arg(long, default_value = "all")]
        channel: String,
    },

    /// Encrypt a secret value
    Encrypt {
        /// Value to encrypt
        #[arg(long)]
        value: String,
    },

    /// List all registered sites
    Sites,

    /// List messages for a site
    Messages {
        /// Site ID (omit for all sites)
        #[arg(long)]
        site_id: Option<String>,

        /// Filter by status (pending, processing, sent, failed, partial_sent)
        #[arg(long, short)]
        status: Option<String>,

        /// Filter by message type (contact_form, newsletter, alert, etc.)
        #[arg(long, short = 't')]
        message_type: Option<String>,

        /// Number of messages to show
        #[arg(long, short, default_value = "20")]
        limit: usize,

        /// Output format (table, json, csv)
        #[arg(long, short, default_value = "table")]
        format: String,

        /// Show full message details
        #[arg(long)]
        verbose: bool,
    },

    /// Show statistics for a site
    Stats {
        /// Site ID (omit for global stats)
        #[arg(long)]
        site_id: Option<String>,

        /// Output format (table, json)
        #[arg(long, short, default_value = "table")]
        format: String,
    },

    /// Show details of a specific message
    Message {
        /// Site ID
        #[arg(long)]
        site_id: String,

        /// Message ID
        #[arg(long)]
        message_id: String,

        /// Output format (table, json)
        #[arg(long, short, default_value = "table")]
        format: String,
    },

    /// Retry a failed message
    Retry {
        /// Site ID
        #[arg(long)]
        site_id: String,

        /// Message ID
        #[arg(long)]
        message_id: String,
    },

    /// Run database cleanup based on retention policies
    Cleanup {
        /// Site ID (omit for all sites)
        #[arg(long)]
        site_id: Option<String>,

        /// Override max age in days
        #[arg(long)]
        max_age_days: Option<u32>,

        /// Delete spam immediately (override config)
        #[arg(long)]
        delete_spam: bool,

        /// Run vacuum after cleanup
        #[arg(long, default_value = "true")]
        vacuum: bool,

        /// Dry run (show what would be deleted without deleting)
        #[arg(long)]
        dry_run: bool,
    },

    /// Show database statistics
    DbStats {
        /// Site ID (omit for all sites)
        #[arg(long)]
        site_id: Option<String>,

        /// Output format (table, json)
        #[arg(long, short, default_value = "table")]
        format: String,
    },
}

/// Configuration loaded from file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// ValKey connection settings
    #[serde(default)]
    pub valkey: ValKeyConfig,

    /// Consumer settings
    #[serde(default)]
    pub consumer: ConsumerConfig,

    /// Default retry settings
    #[serde(default)]
    pub retry: RetryConfig,

    /// Default spam filter settings
    #[serde(default)]
    pub spam_filter: SpamFilterConfig,

    /// Message retention and cleanup settings
    #[serde(default)]
    pub retention: crate::persistence::RetentionConfig,

    /// Inbound message processing (Telegram polling, command routing)
    #[serde(default)]
    pub inbound: Option<InboundConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            valkey: ValKeyConfig::default(),
            consumer: ConsumerConfig::default(),
            retry: RetryConfig::default(),
            spam_filter: SpamFilterConfig::default(),
            retention: crate::persistence::RetentionConfig::default(),
            inbound: None,
        }
    }
}

/// Inbound processing configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InboundConfig {
    /// Telegram bot token for inbound polling
    pub bot_token: Option<String>,
    /// Site ID to bind inbound to (default: "geodine")
    pub site_id: Option<String>,
    /// Default pipeline for new conversations (default: "sysadmin")
    pub default_pipeline: Option<String>,
    /// Authorized Telegram user IDs (empty = allow all)
    pub admin_ids: Option<Vec<i64>>,
    /// Enable the staged inference chain (draft → review → guard)
    /// When false, uses single-pipeline direct inference (default)
    #[serde(default)]
    pub inference_chain_enabled: Option<bool>,
}

impl InboundConfig {
    /// Merge environment variables into the config. Env vars take precedence
    /// over YAML values, allowing secrets to live outside the repo.
    ///
    /// Env vars:
    ///   TELEGRAM_BOT_TOKEN    — bot token from @BotFather
    ///   COMMS_INBOUND_SITE    — site_id to bind to (default "geodine")
    ///   COMMS_INBOUND_PIPELINE — default pipeline (default "sysadmin")
    ///   COMMS_ADMIN_IDS       — comma-separated Telegram user IDs
    ///   COMMS_INFERENCE_CHAIN — "true" to enable the staged chain
    pub fn merge_env(&mut self) {
        if let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN") {
            if !token.is_empty() {
                self.bot_token = Some(token);
            }
        }
        if let Ok(site) = std::env::var("COMMS_INBOUND_SITE") {
            if !site.is_empty() {
                self.site_id = Some(site);
            }
        }
        if let Ok(pipeline) = std::env::var("COMMS_INBOUND_PIPELINE") {
            if !pipeline.is_empty() {
                self.default_pipeline = Some(pipeline);
            }
        }
        // fail-closed on set-but-parses-empty.
        // Pre-fix logic short-circuited both on empty env value AND on
        // parse-yielded-empty — leaving admin_ids at its default (None /
        // "allow-all"). An operator who set COMMS_ADMIN_IDS to `junk,stuff`
        // would unknowingly open the gate to everyone. Post-fix: if the env
        // var is set AT ALL, the admin gate is considered explicitly
        // configured; set admin_ids to Some(parsed) even when parsed is
        // empty so the downstream gate rejects all. Log an error so the
        // operator sees the misconfiguration.
        if let Ok(ids) = std::env::var("COMMS_ADMIN_IDS") {
            let parsed: Vec<i64> = ids
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            if parsed.is_empty() {
                tracing::error!(
                    raw_value_len = ids.len(),
                    "COMMS_ADMIN_IDS is set but parsed to an empty list — \
                     fail-closed: rejecting ALL inbound operators. Fix by \
                     setting a comma-separated list of Telegram user IDs \
                     (e.g. COMMS_ADMIN_IDS=12345,67890), or unset to allow \
                     all (NOT recommended for production)."
                );
            }
            self.admin_ids = Some(parsed);
        }
        if let Ok(v) = std::env::var("COMMS_INFERENCE_CHAIN") {
            self.inference_chain_enabled = Some(v.eq_ignore_ascii_case("true") || v == "1");
        }
    }

    /// Returns true if enough config exists to start inbound polling.
    /// Requires at least a bot_token.
    pub fn is_enabled(&self) -> bool {
        self.bot_token
            .as_ref()
            .map(|t| !t.is_empty())
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValKeyConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: Option<String>,
    pub password: Option<String>,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    47445
}

impl Default for ValKeyConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            user: None,
            password: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsumerConfig {
    #[serde(default = "default_block_ms")]
    pub block_ms: u64,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    #[serde(default = "default_idle_claim_ms")]
    pub idle_claim_ms: u64,
    #[serde(default = "default_discovery_interval_secs")]
    pub discovery_interval_secs: u64,
}

fn default_block_ms() -> u64 {
    5000
}

fn default_batch_size() -> usize {
    100
}

fn default_idle_claim_ms() -> u64 {
    30000
}

fn default_discovery_interval_secs() -> u64 {
    60
}

impl Default for ConsumerConfig {
    fn default() -> Self {
        Self {
            block_ms: default_block_ms(),
            batch_size: default_batch_size(),
            idle_claim_ms: default_idle_claim_ms(),
            discovery_interval_secs: default_discovery_interval_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_base_delay_secs")]
    pub base_delay_secs: u64,
    #[serde(default = "default_max_delay_secs")]
    pub max_delay_secs: u64,
}

fn default_max_attempts() -> u32 {
    5
}

fn default_base_delay_secs() -> u64 {
    30
}

fn default_max_delay_secs() -> u64 {
    3600
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            base_delay_secs: default_base_delay_secs(),
            max_delay_secs: default_max_delay_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpamFilterConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub keywords_blocklist: Vec<String>,
    #[serde(default)]
    pub ip_blocklist: Vec<String>,
}

impl Default for SpamFilterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            keywords_blocklist: vec![],
            ip_blocklist: vec![],
        }
    }
}

impl Config {
    /// Load configuration from file and merge env var overrides.
    ///
    /// Precedence: env vars > YAML file > built-in defaults.
    /// Secrets (bot_token, admin_ids) should be supplied via env file
    /// at /etc/geodineum/components/geodineum-comms/geodineum-comms.env
    /// rather than committed to the YAML.
    pub fn load(path: &PathBuf) -> anyhow::Result<Self> {
        let mut config = if path.exists() {
            let contents = std::fs::read_to_string(path)?;
            serde_yaml::from_str::<Config>(&contents)?
        } else {
            tracing::warn!("Config file not found at {:?}, using defaults", path);
            Config::default()
        };

        // Apply inbound env var overrides (TELEGRAM_BOT_TOKEN, COMMS_ADMIN_IDS, etc.)
        // If inbound section was absent from YAML, create an empty one so env vars
        // alone can activate inbound polling.
        let mut inbound = config.inbound.unwrap_or_default();
        inbound.merge_env();

        // Only keep the inbound config if it's actually enabled (has a bot_token)
        config.inbound = if inbound.is_enabled() {
            Some(inbound)
        } else {
            None
        };

        Ok(config)
    }
}
