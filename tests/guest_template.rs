//! The guest email template: a personal message a site's own staff approved, in the site's frame. Its HTML body
//! arrives cleaned (router::links::clean_html); the frame's own fields are escaped here.

use geodineum_comms::templates::TemplateRenderer;
use tera::Context;

fn ctx(metadata: serde_json::Value, body: &str) -> Context {
    let mut c = Context::new();
    c.insert("content", &serde_json::json!({"subject": "Dank voor uw bezoek", "body": body}));
    c.insert("metadata", &metadata);
    c
}

#[tokio::test]
async fn the_body_in_the_sites_frame() {
    let r = TemplateRenderer::new(concat!(env!("CARGO_MANIFEST_DIR"), "/config/templates")).expect("templates load");
    let meta = serde_json::json!({"brand": "Twenty Seven", "lang": "en", "html": "<p style=\"margin:0 0 16px\">Dear Sanne,</p>",
        "logo_url": "https://twentyseven.innovagent.nl/assets/logo-27-mail.png", "footer": "Hotel <Twenty Seven> · Dam 27"});
    let out = r.render("email/guest", &ctx(meta, "Dear Sanne,")).await.expect("renders");
    assert_eq!(out.subject.as_deref(), Some("Dank voor uw bezoek"));
    let html = out.body_html.expect("html part");
    assert!(html.contains("<html lang=\"en\">"), "{html}");
    assert!(html.contains("<p style=\"margin:0 0 16px\">Dear Sanne,</p>"), "{html}");
    assert!(html.contains("logo-27-mail.png") && html.contains("background:#050505"), "{html}");
    assert!(html.contains("Hotel &lt;Twenty Seven&gt; · Dam 27"), "the footer is escaped: {html}");
    assert!(out.body.contains("Dear Sanne,"), "a plain text part too: {}", out.body);
}

#[tokio::test]
async fn without_html_the_text_escaped_and_no_logo_the_brand() {
    let r = TemplateRenderer::new(concat!(env!("CARGO_MANIFEST_DIR"), "/config/templates")).expect("templates load");
    let out = r.render("email/guest", &ctx(serde_json::json!({"brand": "Twenty Seven"}), "Beste <Sanne>,\n\nDank")).await.expect("renders");
    let html = out.body_html.expect("html part");
    assert!(html.contains("Beste &lt;Sanne&gt;,<br>"), "{html}");
    assert!(html.contains(">Twenty Seven</span>") && !html.contains("<img"), "{html}");
}
