//! A Telegram account linked to a site's web subscriber (the Geodine window): the bot continues the
//! subscriber's web conversation instead of opening one of its own.
//!
//! The site makes a one-time code under its own namespace and the person sends it to the bot
//! (`/start <site>-<CODE>`, the deep link, or `/link <site>-<CODE>`). COMMS records the account here,
//! in the operator site's namespace, which only COMMS writes: that record, not anything the site
//! wrote, is the authority on which chat belongs to which consumer of which site.
//!
//!   {site}:gnode:comms:link:<CODE>      the site's code {consumer, expires_ms}; answered with {consumer, state: linked, operator, chat, name, at_ms}
//!   {site}:gnode:comms:tg:<consumer>    the site's route {consumer, lane, pipeline, session_id, system?, system_mode?, system_name?}; COMMS adds operator, chat
//!   {link}:gnode:comms:web:<operator>   COMMS's record {site, consumer, chat, at_ms}
//!
//! A linked person's message is a `direct` on the SITE's own command stream with `_rt geodine`: the
//! daemon checks the site's reach, the reply comes home to the site, and the site copies both turns
//! into its conversation. COMMS never sends on the site's behalf what the site could not send itself.

use once_cell::sync::Lazy;
use redis::aio::MultiplexedConnection;
use regex::Regex;
use serde::{Deserialize, Serialize};

static SITE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-z][a-z0-9_]{1,62}$").unwrap());
static CODE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[A-Z2-9]{10}$").unwrap());
static CONSUMER_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-z]+:[0-9a-f]{16}$").unwrap());
static LANE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-z][a-z0-9_]{0,48}(\.[0-9a-f]{8})?$").unwrap());

/// The longest message relayed for a linked person (the window's own cap).
pub const PROMPT_MAX: usize = 8000;
/// Messages a linked account may send in an hour; beyond it the bot says so and relays nothing.
pub const PER_HOUR: i64 = 60;
pub const LINKED_TTL_SECS: u64 = 86_400;

/// `(site, code)` from `/start <site>-<CODE>` or `/link <site>-<CODE>`, or None.
pub fn parse_link(text: &str) -> Option<(String, String)> {
    let mut parts = text.split_whitespace();
    let cmd = parts.next()?;
    let cmd = cmd.split('@').next().unwrap_or(cmd);
    if cmd != "/start" && cmd != "/link" {
        return None;
    }
    let payload = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let (site, code) = payload.rsplit_once('-')?;
    let code = code.to_ascii_uppercase();
    (SITE_RE.is_match(site) && CODE_RE.is_match(&code)).then(|| (site.to_string(), code))
}

pub fn is_unlink(text: &str) -> bool {
    let t = text.trim();
    t.split('@').next() == Some("/unlink") && !t.contains(' ')
}

pub fn link_key(site: &str, code: &str) -> String {
    format!("{{{}}}:gnode:comms:link:{}", site, code)
}

pub fn route_key(site: &str, consumer: &str) -> String {
    format!("{{{}}}:gnode:comms:tg:{}", site, consumer)
}

pub fn web_key(link_site: &str, operator: &str) -> String {
    format!("{{{}}}:gnode:comms:web:{}", link_site, operator)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebLink {
    pub site: String,
    pub consumer: String,
    pub chat: String,
    pub at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Route {
    pub consumer: String,
    pub lane: String,
    pub pipeline: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
}

impl Route {
    /// A route the site may not have meant (or may have forged) is not followed.
    pub fn valid(&self, consumer: &str) -> bool {
        self.consumer == consumer
            && CONSUMER_RE.is_match(&self.consumer)
            && LANE_RE.is_match(&self.lane)
            && SITE_RE.is_match(&self.pipeline)
            && self.session_id.len() <= 64
            && self.session_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }
}

#[derive(Debug, Deserialize)]
struct Code {
    consumer: String,
    #[serde(default)]
    expires_ms: i64,
    #[serde(default)]
    state: Option<String>,
}

/// The `direct` a linked person's message becomes, on the site's own command stream: the fields
/// the daemon's relay reads, plus `via`, `cs` and `lane` for the site to find its own turns.
pub fn command_fields(site: &str, node: &str, route: &Route, prompt: &str, request_id: &str, ts_ms: i64) -> Vec<(String, String)> {
    let prompt: String = prompt.chars().take(PROMPT_MAX).collect();
    let mut params = serde_json::json!({
        "pipeline": route.pipeline,
        "consumer": route.consumer,
        "session_id": route.session_id,
    });
    match (route.system.as_deref(), route.system_mode.as_deref()) {
        (Some(text), Some("prefixed")) => {
            let name = route.system_name.as_deref().unwrap_or("preset");
            params["prompt"] = serde_json::Value::String(format!("[{}]\n{}\n\n---\n\n{}", name, text, prompt));
        }
        (Some(text), _) => {
            params["prompt"] = serde_json::Value::String(prompt);
            params["system"] = serde_json::Value::String(text.to_string());
        }
        (None, _) => params["prompt"] = serde_json::Value::String(prompt),
    }
    vec![
        ("t".into(), "c".into()),
        ("c".into(), "direct".into()),
        ("p".into(), params.to_string()),
        ("id".into(), request_id.into()),
        ("ss".into(), site.into()),
        ("sn".into(), node.into()),
        ("ts".into(), ts_ms.to_string()),
        ("_rt".into(), "geodine".into()),
        ("via".into(), "telegram".into()),
        ("cs".into(), route.consumer.clone()),
        ("lane".into(), route.lane.clone()),
    ]
}

/// The site's unified command stream, where a linked person's message goes.
pub fn site_stream(site: &str, environment: &str) -> String {
    format!("{{{}}}:gnode:unified:{}", site, environment)
}

async fn get_json<T: serde::de::DeserializeOwned>(conn: &mut MultiplexedConnection, key: &str) -> Option<T> {
    let raw: Option<String> = redis::cmd("GET").arg(key).query_async(conn).await.ok().flatten();
    raw.and_then(|r| serde_json::from_str(&r).ok())
}

/// Whether this Telegram account is linked to any site's subscriber (the receiver's gate).
pub async fn is_linked(conn: &mut MultiplexedConnection, link_site: &str, operator: &str) -> bool {
    let n: i64 = redis::cmd("EXISTS").arg(web_key(link_site, operator)).query_async(conn).await.unwrap_or(0);
    n == 1
}

#[derive(Debug, PartialEq)]
pub enum LinkOutcome {
    Linked { site: String, lane: String },
    Expired,
}

/// Spend a site's code: record the account, answer the site, put the account on its route.
#[allow(clippy::too_many_arguments)]
pub async fn link(
    conn: &mut MultiplexedConnection,
    link_site: &str,
    site: &str,
    code: &str,
    operator: &str,
    chat: &str,
    name: &str,
    now_ms: i64,
) -> LinkOutcome {
    let key = link_key(site, code);
    let Some(c) = get_json::<Code>(conn, &key).await else { return LinkOutcome::Expired };
    if c.state.is_some() || (c.expires_ms > 0 && c.expires_ms < now_ms) || !CONSUMER_RE.is_match(&c.consumer) {
        return LinkOutcome::Expired;
    }
    let Some(mut route) = get_json::<Route>(conn, &route_key(site, &c.consumer)).await else { return LinkOutcome::Expired };
    if !route.valid(&c.consumer) {
        return LinkOutcome::Expired;
    }
    // One web identity per Telegram account: a previous link of this account to another consumer ends here.
    if let Some(old) = get_json::<WebLink>(conn, &web_key(link_site, operator)).await {
        if old.site != site || old.consumer != c.consumer {
            let _: redis::RedisResult<()> = redis::cmd("DEL").arg(route_key(&old.site, &old.consumer)).query_async(conn).await;
        }
    }
    let web = WebLink { site: site.into(), consumer: c.consumer.clone(), chat: chat.into(), at_ms: now_ms };
    let _: redis::RedisResult<()> = redis::cmd("SET").arg(web_key(link_site, operator)).arg(serde_json::to_string(&web).unwrap_or_default()).query_async(conn).await;
    route.operator = Some(operator.into());
    route.chat = Some(chat.into());
    let _: redis::RedisResult<()> = redis::cmd("SET").arg(route_key(site, &c.consumer)).arg(serde_json::to_string(&route).unwrap_or_default()).query_async(conn).await;
    let answer = serde_json::json!({"consumer": c.consumer, "state": "linked", "operator": operator, "chat": chat,
        "name": name.chars().take(60).collect::<String>(), "at_ms": now_ms});
    let _: redis::RedisResult<()> = redis::cmd("SET").arg(&key).arg(answer.to_string()).arg("EX").arg(LINKED_TTL_SECS).query_async(conn).await;
    LinkOutcome::Linked { site: site.into(), lane: route.lane }
}

/// Where a linked account's message goes: its record and the site's current route, or None when the
/// site no longer routes it to this account (unlinked in the window), in which case the record goes too.
pub async fn resolve(conn: &mut MultiplexedConnection, link_site: &str, operator: &str) -> Option<Result<(WebLink, Route), WebLink>> {
    let web = get_json::<WebLink>(conn, &web_key(link_site, operator)).await?;
    match get_json::<Route>(conn, &route_key(&web.site, &web.consumer)).await {
        Some(r) if r.valid(&web.consumer) && r.operator.as_deref() == Some(operator) => Some(Ok((web, r))),
        _ => {
            let _: redis::RedisResult<()> = redis::cmd("DEL").arg(web_key(link_site, operator)).query_async(conn).await;
            Some(Err(web))
        }
    }
}

/// One more message of this account this hour, or false when it is over PER_HOUR.
pub async fn within_rate(conn: &mut MultiplexedConnection, link_site: &str, operator: &str, now_ms: i64) -> bool {
    let key = format!("{{{}}}:gnode:comms:rate:{}:{}", link_site, operator, now_ms / 3_600_000);
    let n: i64 = redis::cmd("INCR").arg(&key).query_async(conn).await.unwrap_or(0);
    if n == 1 {
        let _: redis::RedisResult<()> = redis::cmd("EXPIRE").arg(&key).arg(3_700).query_async(conn).await;
    }
    n <= PER_HOUR
}

/// `/unlink` from Telegram: the record and the site's route go; the site sees the route gone.
pub async fn unlink(conn: &mut MultiplexedConnection, link_site: &str, operator: &str) -> bool {
    let Some(web) = get_json::<WebLink>(conn, &web_key(link_site, operator)).await else { return false };
    if let Some(r) = get_json::<Route>(conn, &route_key(&web.site, &web.consumer)).await {
        if r.operator.as_deref() == Some(operator) {
            let _: redis::RedisResult<()> = redis::cmd("DEL").arg(route_key(&web.site, &web.consumer)).query_async(conn).await;
        }
    }
    let _: redis::RedisResult<()> = redis::cmd("DEL").arg(web_key(link_site, operator)).query_async(conn).await;
    true
}

/// The chat a site's consumer is linked to, for an addressed Telegram message: only when COMMS's own
/// record names this site and consumer, so a site cannot point a message at a chat it does not own.
pub async fn chat_for(conn: &mut MultiplexedConnection, link_site: &str, site: &str, consumer: &str) -> Option<String> {
    if !CONSUMER_RE.is_match(consumer) {
        return None;
    }
    let route = get_json::<Route>(conn, &route_key(site, consumer)).await?;
    let operator = route.operator?;
    let web = get_json::<WebLink>(conn, &web_key(link_site, &operator)).await?;
    (web.site == site && web.consumer == consumer && !web.chat.is_empty()).then_some(web.chat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_names_the_site_and_a_ten_letter_code() {
        assert_eq!(parse_link("/start geodineum_com-K3J9QF2MXA"), Some(("geodineum_com".into(), "K3J9QF2MXA".into())));
        assert_eq!(parse_link("/link geodineum_com-k3j9qf2mxa"), Some(("geodineum_com".into(), "K3J9QF2MXA".into())));
        assert_eq!(parse_link("/start@GeodineBot geodineum_com-K3J9QF2MXA"), Some(("geodineum_com".into(), "K3J9QF2MXA".into())));
        for bad in ["/start", "/start hello", "/link geodineum_com-K3J9QF2MX", "/link geodineum_com-K3J9QF2MX0", "/link Geodineum-K3J9QF2MXA",
            "/link geodineum_com-K3J9QF2MXA extra", "hello geodineum_com-K3J9QF2MXA", "/link ../x-K3J9QF2MXA"] {
            assert_eq!(parse_link(bad), None, "{bad}");
        }
    }

    #[test]
    fn unlink_is_the_bare_command() {
        assert!(is_unlink("/unlink") && is_unlink("/unlink@GeodineBot") && !is_unlink("/unlink now") && !is_unlink("unlink"));
    }

    fn route() -> Route {
        Route { consumer: "web:0123456789abcdef".into(), lane: "sales_chat.0a1b2c3d".into(), pipeline: "sales_chat".into(), session_id: "a1b2c3d4e5f60718".into(),
            system: Some("Answer in three lines.".into()), system_mode: Some("param".into()), system_name: Some("Terse".into()), operator: Some("42".into()), chat: Some("42".into()) }
    }

    #[test]
    fn a_route_is_followed_only_when_it_is_well_formed_and_the_consumers_own() {
        assert!(route().valid("web:0123456789abcdef"));
        assert!(!route().valid("web:ffffffffffffffff"));
        let mut r = route();
        r.session_id = "../../x".into();
        assert!(!r.valid("web:0123456789abcdef"));
        let mut r = route();
        r.lane = "@agent".into();
        assert!(!r.valid("web:0123456789abcdef"));
    }

    #[test]
    fn the_command_is_the_sites_own_relayed_direct() {
        let f: std::collections::HashMap<String, String> = command_fields("geodineum_com", "aesir", &route(), "Is the backup done?", "tg-42-1", 1_800_000_000_000).into_iter().collect();
        assert_eq!(f["t"], "c");
        assert_eq!(f["c"], "direct");
        assert_eq!(f["_rt"], "geodine");
        assert_eq!(f["ss"], "geodineum_com");
        assert_eq!(f["via"], "telegram");
        assert_eq!(f["cs"], "web:0123456789abcdef");
        assert_eq!(f["lane"], "sales_chat.0a1b2c3d");
        let p: serde_json::Value = serde_json::from_str(&f["p"]).unwrap();
        assert_eq!(p["prompt"], "Is the backup done?");
        assert_eq!(p["system"], "Answer in three lines.");
        assert_eq!(p["session_id"], "a1b2c3d4e5f60718");
        assert_eq!(p["consumer"], "web:0123456789abcdef");
    }

    #[test]
    fn a_prefixed_preset_goes_in_front_of_the_question_as_the_window_does() {
        let mut r = route();
        r.system_mode = Some("prefixed".into());
        let f: std::collections::HashMap<String, String> = command_fields("geodineum_com", "aesir", &r, "Q?", "tg-1", 1).into_iter().collect();
        let p: serde_json::Value = serde_json::from_str(&f["p"]).unwrap();
        assert_eq!(p["prompt"], "[Terse]\nAnswer in three lines.\n\n---\n\nQ?");
        assert!(p.get("system").is_none());
    }

    #[test]
    fn a_long_message_is_cut_at_the_windows_cap() {
        let long = "x".repeat(PROMPT_MAX + 50);
        let f: std::collections::HashMap<String, String> = command_fields("s_s", "n", &route(), &long, "tg-1", 1).into_iter().collect();
        let p: serde_json::Value = serde_json::from_str(&f["p"]).unwrap();
        assert_eq!(p["prompt"].as_str().unwrap().len(), PROMPT_MAX);
    }
}
