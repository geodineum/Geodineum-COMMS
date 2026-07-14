//! Exponential backoff tracker for SQLite archive failures
//!
//! When SQLite writes fail, we don't want to spin the CPU retrying constantly.
//! This module tracks failures and implements exponential backoff with a max delay.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Configuration for backoff behavior
#[derive(Debug, Clone)]
pub struct BackoffConfig {
    /// Initial backoff duration after first failure
    pub initial_delay: Duration,
    /// Maximum backoff duration
    pub max_delay: Duration,
    /// Multiplier for exponential backoff (typically 2.0)
    pub multiplier: f64,
    /// Number of consecutive failures before we consider the site "unhealthy"
    pub unhealthy_threshold: u32,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(300), // 5 minutes max
            multiplier: 2.0,
            unhealthy_threshold: 5,
        }
    }
}

/// State for a single site's backoff
#[derive(Debug, Clone)]
struct SiteBackoffState {
    /// Number of consecutive failures
    consecutive_failures: u32,
    /// When the current backoff period ends
    backoff_until: Option<Instant>,
    /// Last error message
    last_error: Option<String>,
    /// Total failures (for stats)
    total_failures: u64,
}

impl Default for SiteBackoffState {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            backoff_until: None,
            last_error: None,
            total_failures: 0,
        }
    }
}

/// Tracks backoff state for SQLite archive operations across sites
#[derive(Debug)]
pub struct BackoffTracker {
    config: BackoffConfig,
    states: HashMap<String, SiteBackoffState>,
}

impl BackoffTracker {
    /// Create a new backoff tracker with default config
    pub fn new() -> Self {
        Self {
            config: BackoffConfig::default(),
            states: HashMap::new(),
        }
    }

    /// Create a new backoff tracker with custom config
    pub fn with_config(config: BackoffConfig) -> Self {
        Self {
            config,
            states: HashMap::new(),
        }
    }

    /// Check if a site is currently in backoff period
    pub fn is_in_backoff(&self, site_id: &str) -> bool {
        if let Some(state) = self.states.get(site_id) {
            if let Some(until) = state.backoff_until {
                return Instant::now() < until;
            }
        }
        false
    }

    /// Get remaining backoff duration for a site (if any)
    pub fn remaining_backoff(&self, site_id: &str) -> Option<Duration> {
        if let Some(state) = self.states.get(site_id) {
            if let Some(until) = state.backoff_until {
                let now = Instant::now();
                if now < until {
                    return Some(until - now);
                }
            }
        }
        None
    }

    /// Record a successful archive - resets backoff state
    pub fn record_success(&mut self, site_id: &str) {
        if let Some(state) = self.states.get_mut(site_id) {
            state.consecutive_failures = 0;
            state.backoff_until = None;
            state.last_error = None;
        }
    }

    /// Record a failed archive - updates backoff state
    pub fn record_failure(&mut self, site_id: &str, error: &str) {
        let state = self.states.entry(site_id.to_string()).or_default();

        state.consecutive_failures += 1;
        state.total_failures += 1;
        state.last_error = Some(error.to_string());

        // Calculate backoff duration using exponential backoff
        let backoff_secs = self.config.initial_delay.as_secs_f64()
            * self.config.multiplier.powi((state.consecutive_failures - 1) as i32);

        let backoff = Duration::from_secs_f64(backoff_secs.min(self.config.max_delay.as_secs_f64()));

        state.backoff_until = Some(Instant::now() + backoff);

        tracing::warn!(
            site_id = %site_id,
            consecutive_failures = state.consecutive_failures,
            backoff_secs = backoff.as_secs(),
            error = %error,
            "Archive failure - entering backoff"
        );
    }

    /// Check if a site is considered unhealthy (many consecutive failures)
    pub fn is_unhealthy(&self, site_id: &str) -> bool {
        self.states
            .get(site_id)
            .map(|s| s.consecutive_failures >= self.config.unhealthy_threshold)
            .unwrap_or(false)
    }

    /// Get the number of consecutive failures for a site
    pub fn consecutive_failures(&self, site_id: &str) -> u32 {
        self.states
            .get(site_id)
            .map(|s| s.consecutive_failures)
            .unwrap_or(0)
    }

    /// Get the last error for a site
    pub fn last_error(&self, site_id: &str) -> Option<&str> {
        self.states
            .get(site_id)
            .and_then(|s| s.last_error.as_deref())
    }

    /// Get stats for all sites
    pub fn stats(&self) -> BackoffStats {
        let mut healthy = 0;
        let mut in_backoff = 0;
        let mut unhealthy = 0;
        let mut total_failures = 0;

        for (site_id, state) in &self.states {
            total_failures += state.total_failures;

            if self.is_unhealthy(site_id) {
                unhealthy += 1;
            } else if self.is_in_backoff(site_id) {
                in_backoff += 1;
            } else {
                healthy += 1;
            }
        }

        BackoffStats {
            total_sites: self.states.len(),
            healthy,
            in_backoff,
            unhealthy,
            total_failures,
        }
    }

    /// Clear backoff state for a site (e.g., after manual intervention)
    pub fn clear(&mut self, site_id: &str) {
        self.states.remove(site_id);
    }

    /// Clear all backoff states
    pub fn clear_all(&mut self) {
        self.states.clear();
    }
}

impl Default for BackoffTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about backoff states
#[derive(Debug, Clone)]
pub struct BackoffStats {
    pub total_sites: usize,
    pub healthy: usize,
    pub in_backoff: usize,
    pub unhealthy: usize,
    pub total_failures: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn test_backoff_progression() {
        let config = BackoffConfig {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(1),
            multiplier: 2.0,
            unhealthy_threshold: 5,
        };

        let mut tracker = BackoffTracker::with_config(config);

        // First failure: 100ms backoff
        tracker.record_failure("site1", "error1");
        assert!(tracker.is_in_backoff("site1"));
        assert_eq!(tracker.consecutive_failures("site1"), 1);

        // Wait for backoff to expire
        sleep(Duration::from_millis(150));
        assert!(!tracker.is_in_backoff("site1"));

        // Second failure: 200ms backoff
        tracker.record_failure("site1", "error2");
        assert_eq!(tracker.consecutive_failures("site1"), 2);

        // Success resets
        tracker.record_success("site1");
        assert_eq!(tracker.consecutive_failures("site1"), 0);
        assert!(!tracker.is_in_backoff("site1"));
    }

    #[test]
    fn test_unhealthy_threshold() {
        let config = BackoffConfig {
            unhealthy_threshold: 3,
            ..Default::default()
        };

        let mut tracker = BackoffTracker::with_config(config);

        tracker.record_failure("site1", "err");
        assert!(!tracker.is_unhealthy("site1"));

        tracker.record_failure("site1", "err");
        assert!(!tracker.is_unhealthy("site1"));

        tracker.record_failure("site1", "err");
        assert!(tracker.is_unhealthy("site1"));
    }
}
