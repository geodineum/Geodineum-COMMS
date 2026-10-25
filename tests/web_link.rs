//! Telegram linked to a site's web subscriber, against a throwaway ValKey:
//! COMMS_TEST_VALKEY_PORT=<port> cargo test --test web_link (tests/valkey.sh starts one). Without it the tests return early.

use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use geodineum_comms::channels::{
    AddressedTo, ChannelConfig, ChannelRegistry, CommsMessage, MessageContent, NotificationChannel, RateLimit, RecipientConfig, RenderedContent,
    SendResult,
};
use geodineum_comms::error::Result;
use geodineum_comms::inbound::web_link::{self, LinkOutcome, Route};
use geodineum_comms::router::MessageDispatcher;
use geodineum_comms::settings::{SettingsStore, SiteSettings};
use geodineum_comms::templates::TemplateRenderer;

#[derive(Default)]
struct Recorder {
    sent: Mutex<Vec<String>>,
}

#[async_trait]
impl NotificationChannel for Recorder {
    fn name(&self) -> &str {
        "telegram"
    }
    async fn send(&self, _m: &CommsMessage, r: &RecipientConfig, _c: &ChannelConfig) -> Result<SendResult> {
        self.sent.lock().push(r.address.get("chat_id").cloned().unwrap_or_default());
        Ok(SendResult { success: true, provider_id: None, metadata: HashMap::new() })
    }
    fn validate_config(&self, _c: &ChannelConfig) -> Result<()> {
        Ok(())
    }
    fn default_rate_limit(&self) -> RateLimit {
        RateLimit { max_requests: 100, window_secs: 60 }
    }
    async fn render_content(&self, _m: &CommsMessage, _t: Option<&str>) -> Result<RenderedContent> {
        Ok(RenderedContent::plain(""))
    }
}

const BOT: &str = "wl_bot";

async fn conn() -> Option<redis::aio::MultiplexedConnection> {
    let port = std::env::var("COMMS_TEST_VALKEY_PORT").ok()?;
    let client = redis::Client::open(format!("redis://127.0.0.1:{}", port)).expect("test ValKey");
    Some(client.get_multiplexed_async_connection().await.expect("test ValKey"))
}

async fn set(c: &mut redis::aio::MultiplexedConnection, k: &str, v: &str) {
    let _: () = redis::cmd("SET").arg(k).arg(v).query_async(c).await.unwrap();
}

async fn get(c: &mut redis::aio::MultiplexedConnection, k: &str) -> Option<String> {
    redis::cmd("GET").arg(k).query_async(c).await.unwrap()
}

/// What the window writes: a code and the route of the lane it links.
async fn window(c: &mut redis::aio::MultiplexedConnection, site: &str, consumer: &str, code: &str, expires_ms: i64) {
    set(c, &web_link::link_key(site, code), &serde_json::json!({"consumer": consumer, "expires_ms": expires_ms}).to_string()).await;
    set(c, &web_link::route_key(site, consumer), &serde_json::json!({"consumer": consumer, "lane": "sysadmin", "pipeline": "sysadmin", "session_id": "a1b2c3d4e5f60718"}).to_string()).await;
}

const NOW: i64 = 1_800_000_000_000;

#[tokio::test]
async fn a_code_links_once_and_the_site_hears_back() {
    let Some(mut c) = conn().await else { return };
    let (site, who) = ("wl_a", "web:00000000000000a1");
    window(&mut c, site, who, "AAAABBBBCC", NOW + 900_000).await;
    let out = web_link::link(&mut c, BOT, site, "AAAABBBBCC", "1001", "1001", "Ana", NOW).await;
    assert_eq!(out, LinkOutcome::Linked { site: site.into(), lane: "sysadmin".into() });
    let answer: serde_json::Value = serde_json::from_str(&get(&mut c, &web_link::link_key(site, "AAAABBBBCC")).await.unwrap()).unwrap();
    assert_eq!(answer["state"], "linked");
    assert_eq!(answer["consumer"], who);
    let route: Route = serde_json::from_str(&get(&mut c, &web_link::route_key(site, who)).await.unwrap()).unwrap();
    assert_eq!(route.operator.as_deref(), Some("1001"));
    assert!(web_link::is_linked(&mut c, BOT, "1001").await);
    assert_eq!(web_link::link(&mut c, BOT, site, "AAAABBBBCC", "1001", "1001", "Ana", NOW).await, LinkOutcome::Expired, "a spent code links nothing");
    let (web, r) = web_link::resolve(&mut c, BOT, "1001").await.unwrap().unwrap();
    assert_eq!((web.site.as_str(), web.consumer.as_str(), r.session_id.as_str()), (site, who, "a1b2c3d4e5f60718"));
}

#[tokio::test]
async fn an_expired_or_unknown_code_links_nothing() {
    let Some(mut c) = conn().await else { return };
    window(&mut c, "wl_b", "web:00000000000000b1", "CCCCDDDDEE", NOW - 1).await;
    assert_eq!(web_link::link(&mut c, BOT, "wl_b", "CCCCDDDDEE", "1002", "1002", "B", NOW).await, LinkOutcome::Expired);
    assert_eq!(web_link::link(&mut c, BOT, "wl_b", "ZZZZZZZZZZ", "1002", "1002", "B", NOW).await, LinkOutcome::Expired);
    assert!(!web_link::is_linked(&mut c, BOT, "1002").await);
}

#[tokio::test]
async fn the_window_unlinking_ends_it_and_the_record_goes() {
    let Some(mut c) = conn().await else { return };
    let (site, who) = ("wl_c", "web:00000000000000c1");
    window(&mut c, site, who, "EEEEFFFFGG", NOW + 900_000).await;
    web_link::link(&mut c, BOT, site, "EEEEFFFFGG", "1003", "1003", "C", NOW).await;
    let _: () = redis::cmd("DEL").arg(web_link::route_key(site, who)).query_async(&mut c).await.unwrap();
    assert!(matches!(web_link::resolve(&mut c, BOT, "1003").await, Some(Err(_))));
    assert!(!web_link::is_linked(&mut c, BOT, "1003").await);
}

#[tokio::test]
async fn unlink_from_telegram_removes_the_sites_route() {
    let Some(mut c) = conn().await else { return };
    let (site, who) = ("wl_d", "web:00000000000000d1");
    window(&mut c, site, who, "GGGGHHHHJJ", NOW + 900_000).await;
    web_link::link(&mut c, BOT, site, "GGGGHHHHJJ", "1004", "1004", "D", NOW).await;
    assert!(web_link::unlink(&mut c, BOT, "1004").await);
    assert!(get(&mut c, &web_link::route_key(site, who)).await.is_none());
    assert!(!web_link::unlink(&mut c, BOT, "1004").await);
}

#[tokio::test]
async fn a_site_cannot_aim_at_a_chat_it_did_not_link() {
    let Some(mut c) = conn().await else { return };
    window(&mut c, "wl_e", "web:00000000000000e1", "KKKKLLLLMM", NOW + 900_000).await;
    web_link::link(&mut c, BOT, "wl_e", "KKKKLLLLMM", "1005", "1005", "E", NOW).await;
    // another site writes a route of its own naming that account
    set(&mut c, &web_link::route_key("wl_f", "web:00000000000000f1"),
        &serde_json::json!({"consumer": "web:00000000000000f1", "lane": "sysadmin", "pipeline": "sysadmin", "session_id": "x", "operator": "1005", "chat": "1005"}).to_string()).await;
    assert_eq!(web_link::chat_for(&mut c, BOT, "wl_e", "web:00000000000000e1").await.as_deref(), Some("1005"));
    assert_eq!(web_link::chat_for(&mut c, BOT, "wl_f", "web:00000000000000f1").await, None);
}

#[tokio::test]
async fn relinking_an_account_ends_its_previous_link() {
    let Some(mut c) = conn().await else { return };
    window(&mut c, "wl_g", "web:00000000000000a7", "MMMMNNNNPP", NOW + 900_000).await;
    web_link::link(&mut c, BOT, "wl_g", "MMMMNNNNPP", "1006", "1006", "G", NOW).await;
    window(&mut c, "wl_g", "web:00000000000000b7", "PPPPQQQQRR", NOW + 900_000).await;
    web_link::link(&mut c, BOT, "wl_g", "PPPPQQQQRR", "1006", "1006", "G", NOW).await;
    assert!(get(&mut c, &web_link::route_key("wl_g", "web:00000000000000a7")).await.is_none());
    assert_eq!(web_link::resolve(&mut c, BOT, "1006").await.unwrap().unwrap().0.consumer, "web:00000000000000b7");
}

async fn dispatcher(c: &mut redis::aio::MultiplexedConnection, bot: &str, site: &str, types: Vec<String>, bot_enabled: bool) -> (MessageDispatcher, Arc<Recorder>) {
    let mut s = SiteSettings::new(site);
    s.addressed.types = types;
    set(c, &format!("{{{}}}:comms:config", site), &serde_json::to_string(&s).unwrap()).await;
    let mut b = SiteSettings::new(bot);
    b.channels.telegram = Some(ChannelConfig {
        enabled: bot_enabled,
        config: HashMap::from([("bot_token".to_string(), serde_json::json!("t")), ("chat_id".to_string(), serde_json::json!("999"))]),
        recipients: vec![RecipientConfig { address: HashMap::from([("chat_id".to_string(), "999".to_string())]), types: vec![], min_priority: 5 }],
        rate_limit: None,
    });
    set(c, &format!("{{{}}}:comms:config", bot), &serde_json::to_string(&b).unwrap()).await;
    let renderer = Arc::new(TemplateRenderer::new("templates").expect("template renderer"));
    let mut registry = ChannelRegistry::new(renderer);
    let rec = Arc::new(Recorder::default());
    registry.register(rec.clone());
    (MessageDispatcher::new(Arc::new(registry), Arc::new(SettingsStore::new(c.clone())), false, false).with_link_site(bot), rec)
}

fn digest(site: &str, consumer: &str) -> CommsMessage {
    CommsMessage {
        id: format!("m-{}", consumer),
        message_type: "alpha".into(),
        timestamp: "2026-10-26T06:00:00Z".into(),
        site_id: site.into(),
        priority: 3,
        sender: None,
        content: MessageContent { subject: Some("Geodine, overnight".into()), body: Some("…".into()), attachments: HashMap::new() },
        metadata: HashMap::new(),
        environment: "production".into(),
        dispatch: None,
        to: Some(AddressedTo { email: String::new(), name: None, telegram: Some(consumer.into()) }),
    }
}

#[tokio::test]
async fn a_digest_reaches_the_linked_chat_and_only_it() {
    let Some(mut c) = conn().await else { return };
    let (site, who) = ("wl_h", "web:00000000000000a8");
    window(&mut c, site, who, "RRRRSSSSTT", NOW + 900_000).await;
    web_link::link(&mut c, "wl_bot_h", site, "RRRRSSSSTT", "1008", "1008", "H", NOW).await;
    let (d, rec) = dispatcher(&mut c, "wl_bot_h", site, vec!["alpha".into()], true).await;
    let r = d.dispatch(&digest(site, who)).await.unwrap();
    assert_eq!(r.successful_channels, vec!["telegram"]);
    assert_eq!(*rec.sent.lock(), vec!["1008"], "the bot's own operator chat must not get it");
    let none = d.dispatch(&digest(site, "web:00000000000000ff")).await.unwrap();
    assert_eq!(none.skipped_channels, vec!["telegram (not_linked)"]);
    assert_eq!(rec.sent.lock().len(), 1);
}

#[tokio::test]
async fn a_digest_needs_the_type_allowed_and_a_bot() {
    let Some(mut c) = conn().await else { return };
    let (site, who) = ("wl_i", "web:00000000000000a9");
    window(&mut c, site, who, "TTTTUUUUVV", NOW + 900_000).await;
    web_link::link(&mut c, "wl_bot_i", site, "TTTTUUUUVV", "1009", "1009", "I", NOW).await;
    let (d, rec) = dispatcher(&mut c, "wl_bot_i", site, vec!["login".into()], true).await;
    assert_eq!(d.dispatch(&digest(site, who)).await.unwrap().skipped_channels, vec!["telegram (addressed_type_refused)"]);
    let (d, _) = dispatcher(&mut c, "wl_bot_i", site, vec!["alpha".into()], false).await;
    assert_eq!(d.dispatch(&digest(site, who)).await.unwrap().skipped_channels, vec!["telegram (no_bot)"]);
    assert!(rec.sent.lock().is_empty());
}

#[tokio::test]
async fn a_linked_account_is_limited_per_hour() {
    let Some(mut c) = conn().await else { return };
    for _ in 0..web_link::PER_HOUR {
        assert!(web_link::within_rate(&mut c, BOT, "1010", NOW).await);
    }
    assert!(!web_link::within_rate(&mut c, BOT, "1010", NOW).await);
    assert!(web_link::within_rate(&mut c, BOT, "1010", NOW + 3_600_000).await, "the next hour starts again");
}
