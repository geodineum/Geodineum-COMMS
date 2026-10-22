//! Addressed messages against a throwaway ValKey: COMMS_TEST_VALKEY_PORT=<port> cargo test --test addressed
//! (tests/valkey.sh starts one). Without the variable the tests return early.

use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use geodineum_comms::channels::{
    AddressedTo, ChannelConfig, ChannelRegistry, CommsMessage, MessageContent, NotificationChannel, RateLimit,
    RecipientConfig, RenderedContent, SendResult,
};
use geodineum_comms::error::Result;
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
        "email"
    }
    async fn send(&self, _m: &CommsMessage, r: &RecipientConfig, _c: &ChannelConfig) -> Result<SendResult> {
        self.sent.lock().push(r.address.get("email").cloned().unwrap_or_default());
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

async fn setup(site: &str, cap: u64, allow_nonprod: bool) -> Option<(MessageDispatcher, Arc<Recorder>)> {
    let port = std::env::var("COMMS_TEST_VALKEY_PORT").ok()?;
    let client = redis::Client::open(format!("redis://127.0.0.1:{}", port)).expect("test ValKey");
    let conn = client.get_multiplexed_async_connection().await.expect("test ValKey");
    let mut settings = SiteSettings::new(site);
    settings.addressed.daily_cap = cap;
    settings.channels.email = Some(ChannelConfig {
        enabled: true,
        config: HashMap::new(),
        recipients: vec![RecipientConfig {
            address: HashMap::from([("email".to_string(), "owner@example.invalid".to_string())]),
            types: vec![],
            min_priority: 5,
        }],
        rate_limit: None,
    });
    let mut c = conn.clone();
    let _: () = redis::cmd("SET")
        .arg(format!("{{{}}}:comms:config", site))
        .arg(serde_json::to_string(&settings).unwrap())
        .query_async(&mut c)
        .await
        .expect("test ValKey");
    let renderer = Arc::new(TemplateRenderer::new("templates").expect("template renderer"));
    let mut registry = ChannelRegistry::new(renderer);
    let recorder = Arc::new(Recorder::default());
    registry.register(recorder.clone());
    Some((
        MessageDispatcher::new(Arc::new(registry), Arc::new(SettingsStore::new(conn)), allow_nonprod, false),
        recorder,
    ))
}

fn message(site: &str, kind: &str, to: Option<&str>, env: &str) -> CommsMessage {
    CommsMessage {
        id: format!("m-{}-{}", kind, to.unwrap_or("none")),
        message_type: kind.to_string(),
        timestamp: "2026-10-22T12:00:00Z".to_string(),
        site_id: site.to_string(),
        priority: 3,
        sender: None,
        content: MessageContent { subject: Some("Your link".into()), body: Some("…".into()), attachments: HashMap::new() },
        metadata: HashMap::new(),
        environment: env.to_string(),
        dispatch: None,
        to: to.map(|e| AddressedTo { email: e.to_string(), name: Some("Ana".into()) }),
    }
}

#[tokio::test]
async fn an_addressed_message_reaches_only_its_person() {
    let Some((d, rec)) = setup("addr_a", 200, false).await else { return };
    let r = d.dispatch(&message("addr_a", "planning", Some("ana@example.invalid"), "production")).await.unwrap();
    assert_eq!(r.successful_channels, vec!["email"]);
    assert_eq!(*rec.sent.lock(), vec!["ana@example.invalid"], "the site's own recipient must not get it");
    let plain = d.dispatch(&message("addr_a", "contact", None, "production")).await.unwrap();
    assert!(plain.is_success());
    assert_eq!(rec.sent.lock().last().unwrap(), "owner@example.invalid", "unaddressed mail still goes to the site");
}

#[tokio::test]
async fn a_type_the_site_does_not_allow_is_refused() {
    let Some((d, rec)) = setup("addr_b", 200, false).await else { return };
    let r = d.dispatch(&message("addr_b", "contact", Some("stranger@example.invalid"), "production")).await.unwrap();
    assert_eq!(r.skipped_channels, vec!["email (addressed_type_refused)"]);
    assert!(rec.sent.lock().is_empty());
}

#[tokio::test]
async fn an_address_carrying_a_header_is_refused() {
    let Some((d, rec)) = setup("addr_c", 200, false).await else { return };
    let r = d.dispatch(&message("addr_c", "planning", Some("ana@example.invalid\r\nBcc: x@example.invalid"), "production")).await.unwrap();
    assert_eq!(r.skipped_channels, vec!["email (addressed_invalid)"]);
    assert!(rec.sent.lock().is_empty());
}

#[tokio::test]
async fn the_daily_cap_holds() {
    let Some((d, rec)) = setup("addr_d", 2, false).await else { return };
    for who in ["a@example.invalid", "b@example.invalid"] {
        assert!(d.dispatch(&message("addr_d", "planning", Some(who), "production")).await.unwrap().is_success());
    }
    let third = d.dispatch(&message("addr_d", "planning", Some("c@example.invalid"), "production")).await.unwrap();
    assert_eq!(third.skipped_channels, vec!["email (addressed_cap)"]);
    assert_eq!(rec.sent.lock().len(), 2);
}

#[tokio::test]
async fn non_production_stays_a_dry_run_and_does_not_count() {
    let Some((d, rec)) = setup("addr_e", 1, false).await else { return };
    let r = d.dispatch(&message("addr_e", "planning", Some("ana@example.invalid"), "staging")).await.unwrap();
    assert_eq!(r.skipped_channels, vec!["email"]);
    assert!(rec.sent.lock().is_empty());
    assert!(d.dispatch(&message("addr_e", "planning", Some("ana@example.invalid"), "production")).await.unwrap().is_success());
}
