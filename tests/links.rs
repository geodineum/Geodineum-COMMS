//! A mail's links against a throwaway ValKey: COMMS_TEST_VALKEY_PORT=<port> cargo test --test links
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
    metadata: Mutex<Vec<HashMap<String, serde_json::Value>>>,
}

#[async_trait]
impl NotificationChannel for Recorder {
    fn name(&self) -> &str {
        "email"
    }
    async fn send(&self, m: &CommsMessage, _r: &RecipientConfig, _c: &ChannelConfig) -> Result<SendResult> {
        self.metadata.lock().push(m.metadata.clone());
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

/// `site_listed`: what the site writes into its own settings as `link_domains`; `operator_listed`: the
/// operators' file. Only the second may count.
async fn setup(site: &str, site_listed: &[&str], operator_listed: &[&str]) -> Option<(MessageDispatcher, Arc<Recorder>)> {
    let port = std::env::var("COMMS_TEST_VALKEY_PORT").ok()?;
    let client = redis::Client::open(format!("redis://127.0.0.1:{}", port)).expect("test ValKey");
    let conn = client.get_multiplexed_async_connection().await.expect("test ValKey");
    let mut settings = SiteSettings::new(site);
    settings.addressed.types = vec!["shop".to_string()];
    settings.channels.email = Some(ChannelConfig {
        enabled: true,
        config: HashMap::new(),
        recipients: vec![],
        rate_limit: None,
    });
    let mut written = serde_json::to_value(&settings).unwrap();
    written["link_domains"] = serde_json::json!(site_listed);
    let mut c = conn.clone();
    let _: () = redis::cmd("SET")
        .arg(format!("{{{}}}:comms:config", site))
        .arg(written.to_string())
        .query_async(&mut c)
        .await
        .expect("test ValKey");
    let file = std::env::temp_dir().join(format!("comms-link-domains-{}-{}", std::process::id(), site));
    let mut body = format!("# operators\nother_site {}\n", site_listed.join(" "));
    if !operator_listed.is_empty() {
        body.push_str(&format!("{} {}\n", site, operator_listed.join(" ")));
    }
    std::fs::write(&file, body).expect("operators' file");
    let renderer = Arc::new(TemplateRenderer::new("templates").expect("template renderer"));
    let mut registry = ChannelRegistry::new(renderer);
    let recorder = Arc::new(Recorder::default());
    registry.register(recorder.clone());
    let d = MessageDispatcher::new(Arc::new(registry), Arc::new(SettingsStore::new(conn)), false, false)
        .with_link_domains_file(file);
    Some((d, recorder))
}

fn shop_mail(site: &str, cta: &str) -> CommsMessage {
    CommsMessage {
        id: format!("m-{}", cta.len()),
        message_type: "shop".to_string(),
        timestamp: "2026-11-17T12:00:00Z".to_string(),
        site_id: site.to_string(),
        priority: 2,
        sender: None,
        content: MessageContent { subject: Some("Your order".into()), body: Some("…".into()), attachments: HashMap::new() },
        metadata: HashMap::from([
            ("cta_url".to_string(), serde_json::json!(cta)),
            ("cta_label".to_string(), serde_json::json!("Pay")),
            ("site_url".to_string(), serde_json::json!(format!("https://{}/", site.replace('_', ".")))),
        ]),
        environment: "production".to_string(),
        dispatch: None,
        to: Some(AddressedTo { email: "buyer@example.invalid".into(), name: None, telegram: None, operator: false }),
    }
}

#[tokio::test]
async fn a_button_to_another_host_is_dropped_before_it_reaches_the_channel() {
    let Some((d, rec)) = setup("links_a_nl", &[], &[]).await else { return };
    assert!(d.dispatch(&shop_mail("links_a_nl", "https://pay.evil.example/links_a_nl")).await.unwrap().is_success());
    let got = rec.metadata.lock().last().cloned().unwrap();
    assert!(!got.contains_key("cta_url"), "a foreign button must not reach the mail");
    assert_eq!(got.get("site_url"), Some(&serde_json::json!("https://links.a.nl/")), "the site's own link stays");
}

#[tokio::test]
async fn the_sites_own_button_and_a_domain_the_operators_list_pass() {
    let Some((d, rec)) = setup("links_b_nl", &[], &["shop-b.example"]).await else { return };
    for cta in ["https://links.b.nl/order?t=x", "https://www.shop-b.example/order?t=x"] {
        assert!(d.dispatch(&shop_mail("links_b_nl", cta)).await.unwrap().is_success());
        assert_eq!(rec.metadata.lock().last().unwrap().get("cta_url"), Some(&serde_json::json!(cta)));
    }
}

#[tokio::test]
async fn a_domain_the_site_lists_in_its_own_settings_does_not_count() {
    let Some((d, rec)) = setup("links_c_nl", &["pay.evil.example"], &[]).await else { return };
    assert!(d.dispatch(&shop_mail("links_c_nl", "https://pay.evil.example/links_c_nl")).await.unwrap().is_success());
    let got = rec.metadata.lock().last().cloned().unwrap();
    assert!(!got.contains_key("cta_url"), "the site's own link_domains must not let a foreign button through");
}

#[tokio::test]
async fn another_sites_listed_domain_does_not_count() {
    let Some((d, rec)) = setup("links_d_nl", &["shop-d.example"], &["own-d.example"]).await else { return };
    assert!(d.dispatch(&shop_mail("links_d_nl", "https://shop-d.example/x")).await.unwrap().is_success());
    assert!(!rec.metadata.lock().last().unwrap().contains_key("cta_url"), "a domain listed for other_site is not this site's");
    assert!(d.dispatch(&shop_mail("links_d_nl", "https://own-d.example/x")).await.unwrap().is_success());
    assert_eq!(rec.metadata.lock().last().unwrap().get("cta_url"), Some(&serde_json::json!("https://own-d.example/x")));
}
