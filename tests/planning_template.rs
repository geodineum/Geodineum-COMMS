//! The planning email template: branded from the message's metadata, the body escaped with its
//! line breaks kept, and a plain message without metadata still renders.

use geodineum_comms::templates::TemplateRenderer;
use tera::Context;

fn ctx(metadata: serde_json::Value, body: &str) -> Context {
    let mut c = Context::new();
    c.insert("content", &serde_json::json!({"subject": "Je planning-demo", "body": body}));
    c.insert("metadata", &metadata);
    c
}

#[tokio::test]
async fn branded_from_metadata_body_escaped() {
    let r = TemplateRenderer::new(concat!(env!("CARGO_MANIFEST_DIR"), "/config/templates")).expect("templates load");
    let out = r
        .render(
            "email/planning",
            &ctx(
                serde_json::json!({"brand": "Innovagent", "brand_color": "#043B55", "accent_color": "#33D6A5",
                    "logo_url": "https://innovagent.nl/assets/brand/logo-480.png", "cta_url": "https://innovagent.nl/demo/planning",
                    "cta_label": "Open de demo", "site_url": "https://innovagent.nl/", "footer": "Nierto Solutions · KVK 82716099"}),
                "Hallo,\n\nKlik <hier>\nhttps://innovagent.nl/demo/planning",
            ),
        )
        .await
        .expect("renders");
    assert_eq!(out.subject.as_deref(), Some("Je planning-demo"));
    let html = out.body_html.expect("html part");
    assert!(html.contains("background:#043B55"));
    // Tera escapes "/" in attributes as &#x2F;, which every HTML parser decodes.
    assert!(html.contains("href=\"https:&#x2F;&#x2F;innovagent.nl&#x2F;demo&#x2F;planning\""), "{html}");
    assert!(html.contains("Klik &lt;hier&gt;<br>"), "body escaped, breaks kept: {html}");
    assert!(html.contains("KVK 82716099"));
    assert!(out.body.contains("https://innovagent.nl/demo/planning"), "the text part keeps the link");
}

#[tokio::test]
async fn plain_planning_mail_without_metadata() {
    let r = TemplateRenderer::new(concat!(env!("CARGO_MANIFEST_DIR"), "/config/templates")).expect("templates load");
    let out = r.render("email/planning", &ctx(serde_json::json!({}), "Your code: 123456")).await.expect("renders");
    let html = out.body_html.expect("html part");
    assert!(html.contains("Your code: 123456"));
    assert!(!html.contains("href=\"\""), "no empty button");
}
