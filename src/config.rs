//! Configuration management

use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// GSD-COMMS Notification Daemon
#[derive(Parser, Debug)]
#[command(name = "gsd-comms")]
#[command(about = "Notification daemon for GSD - dispatches notifications via email, Telegram, and SMS")]
#[command(version)]
pub struct Cli {
    /// ValKey host
    #[arg(long, default_value = "127.0.0.1", env = "VALKEY_HOST")]
    pub redis_host: String,

    /// ValKey port
    #[arg(long, default_value = "47445", env = "VALKEY_PORT")]
    pub redis_port: u16,

    /// ValKey ACL username
    #[arg(long, default_value = "gsd_comms", env = "VALKEY_USER")]
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

    /// API server port
    #[arg(long, default_value = "8080", env = "API_PORT")]
    pub api_port: u16,

    /// API server bind address
    #[arg(long, default_value = "127.0.0.1", env = "API_BIND")]
    pub api_bind: String,

    /// Number of worker threads (0 = auto)
    #[arg(long, default_value = "0", env = "WORKERS")]
    pub workers: usize,

    /// Unique node identifier
    #[arg(long, env = "NODE_ID")]
    pub node_id: Option<String>,

    /// DTAP environment filter
    #[arg(long, default_value = "production", env = "ENVIRONMENT")]
    pub environment: String,

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
}

/// Configuration loaded from file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// ValKey connection settings
    #[serde(default)]
    pub valkey: ValKeyConfig,

    /// API server settings
    #[serde(default)]
    pub api: ApiConfig,

    /// Consumer settings
    #[serde(default)]
    pub consumer: ConsumerConfig,

    /// Default retry settings
    #[serde(default)]
    pub retry: RetryConfig,

    /// Default spam filter settings
    #[serde(default)]
    pub spam_filter: SpamFilterConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            valkey: ValKeyConfig::default(),
            api: ApiConfig::default(),
            consumer: ConsumerConfig::default(),
            retry: RetryConfig::default(),
            spam_filter: SpamFilterConfig::default(),
        }
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
pub struct ApiConfig {
    #[serde(default = "default_api_bind")]
    pub bind: String,
    #[serde(default = "default_api_port")]
    pub port: u16,
    #[serde(default)]
    pub enable_dashboard: bool,
}

fn default_api_bind() -> String {
    "127.0.0.1".to_string()
}

fn default_api_port() -> u16 {
    8080
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            bind: default_api_bind(),
            port: default_api_port(),
            enable_dashboard: true,
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
    /// Load configuration from file
    pub fn load(path: &PathBuf) -> anyhow::Result<Self> {
        if path.exists() {
            let contents = std::fs::read_to_string(path)?;
            let config: Config = serde_yaml::from_str(&contents)?;
            Ok(config)
        } else {
            tracing::warn!("Config file not found at {:?}, using defaults", path);
            Ok(Config::default())
        }
    }
}
