//! The onboarding email template: staff sign-in codes and invitations in the tenant's brand and language.

use geodineum_comms::templates::TemplateRenderer;
use tera::Context;

fn ctx(metadata: serde_json::Value, body: &str) -> Context {
    let mut c = Context::new();
    c.insert("content", &serde_json::json!({"subject": "Je code: K7QM-2XPA", "body": body}));
    c.insert("metadata", &metadata);
    c
}

#[tokio::test]
async fn code_in_the_tenants_brand_and_language() {
    let r = TemplateRenderer::new(concat!(env!("CARGO_MANIFEST_DIR"), "/config/templates")).expect("templates load");
    let out = r
        .render(
            "email/onboarding",
            &ctx(
                serde_json::json!({"brand": "Hotel", "brand_color": "#1d2b36", "accent_color": "#b08d57", "lang": "en",
                    "code": "K7QM-2XPA", "cta_url": "https://hotel.example/team/link?t=abc", "cta_label": "Sign in",
                    "site_url": "https://hotel.example/", "footer": "Hotel BV · KVK 1"}),
                "Hi <Ana>,\n\nYour code is K7QM-2XPA",
            ),
        )
        .await
        .expect("renders");
    assert_eq!(out.subject.as_deref(), Some("Je code: K7QM-2XPA"));
    let html = out.body_html.expect("html part");
    assert!(html.contains("<html lang=\"en\">"), "{html}");
    assert!(html.contains("background:#1d2b36") && html.contains("background:#b08d57"));
    assert!(html.contains("letter-spacing:0.12em;color:#1d2b36") && html.contains(">K7QM-2XPA</span>"), "the code stands out: {html}");
    assert!(html.contains("Hi &lt;Ana&gt;,<br>"), "body escaped, breaks kept");
    assert!(html.contains("team&#x2F;link?t=abc"));
}

#[tokio::test]
async fn an_invitation_without_code_or_language() {
    let r = TemplateRenderer::new(concat!(env!("CARGO_MANIFEST_DIR"), "/config/templates")).expect("templates load");
    let out = r.render("email/onboarding", &ctx(serde_json::json!({"brand": "Hotel"}), "Welkom")).await.expect("renders");
    let html = out.body_html.expect("html part");
    assert!(html.contains("<html lang=\"nl\">") && html.contains("Welkom"));
    assert!(!html.contains("letter-spacing:0.12em") && !html.contains("href=\"\""), "no empty code or button");
}
