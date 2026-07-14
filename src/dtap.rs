//! Canonical DTAP environment semantics for COMMS.
//!
//! Mirrors `gNode/daemon/config/dtap_schema.yaml`: every tier except
//! `production` (development, testing, staging, acceptance) is non-production
//! and must NOT trigger real side-effects (email/SMS/Telegram) unless the
//! operator explicitly opts in. This module is the ONE place that answers
//! "is this production?" for the daemon — no per-call-site string compares.

/// The only environment that permits real sends. Case-insensitive so a
/// producer's `Production`/`PRODUCTION` still counts; everything else
/// (including an empty or unknown value) is treated as non-production —
/// fail-safe by construction.
pub fn is_production(environment: &str) -> bool {
    environment.eq_ignore_ascii_case("production")
}

/// Extract the DTAP environment from a COMMS stream key of the form
/// `{site_id}:gnode:comms:<env>` → `<env>`. Returns `None` when the suffix is
/// absent or empty so the caller can fall back to a fail-safe default.
pub fn environment_from_stream_key(stream_key: &str) -> Option<&str> {
    stream_key
        .split(":gnode:comms:")
        .nth(1)
        .map(str::trim)
        .filter(|e| !e.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_production_is_production() {
        assert!(is_production("production"));
        assert!(is_production("Production"));
        assert!(is_production("PRODUCTION"));
        for env in ["development", "testing", "staging", "acceptance", "", "prod", "unknown"] {
            assert!(!is_production(env), "{env} must be non-production");
        }
    }

    #[test]
    fn parses_env_from_stream_key() {
        assert_eq!(environment_from_stream_key("{mysite}:gnode:comms:production"), Some("production"));
        assert_eq!(environment_from_stream_key("{s}:gnode:comms:staging"), Some("staging"));
        assert_eq!(environment_from_stream_key("{s}:gnode:comms:"), None);
        assert_eq!(environment_from_stream_key("garbage"), None);
    }
}
