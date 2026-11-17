//! A mail's button, footer link and logo come from the message's metadata, which more than the site can
//! write to its stream. They are kept only when they point at the sending site itself.

use reqwest::Url;
use tracing::warn;

use crate::channels::CommsMessage;

/// Metadata fields a template turns into a link or an image.
pub const LINK_FIELDS: &[&str] = &["cta_url", "site_url", "logo_url"];

/// The domain a site id names: `innovagent_nl` is innovagent.nl, `rodeloper_innovagent_nl` is
/// rodeloper.innovagent.nl. Only `_` maps back to `.`, so a hyphenated look-alike
/// (rodeloper-innovagent.nl) never matches; a site whose domain has a hyphen lists it in `link_domains`.
pub fn site_domain(site_id: &str) -> Option<String> {
    let d = site_id.trim().to_ascii_lowercase().replace('_', ".");
    (d.contains('.') && !d.starts_with('.') && !d.ends_with('.') && !d.contains("..")).then_some(d)
}

fn norm(d: &str) -> String {
    d.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// https, no credentials, no port, and a host that is the site's domain or a subdomain of it.
pub fn is_own_link(site_id: &str, link_domains: &[String], url: &str) -> bool {
    let Ok(u) = Url::parse(url.trim()) else { return false };
    if u.scheme() != "https" || !u.username().is_empty() || u.password().is_some() || u.port().is_some() {
        return false;
    }
    let Some(host) = u.domain() else { return false };
    let host = norm(host);
    let own = site_domain(site_id).into_iter().chain(link_domains.iter().map(|d| norm(d)));
    own.filter(|d| d.contains('.'))
        .any(|d| host == d || host.ends_with(&format!(".{}", d)))
}

/// The message with every link field that does not point at its own site removed (the template then
/// renders no button, no footer link, no logo), each removal logged.
pub fn own_links_only(message: &CommsMessage, link_domains: &[String]) -> Option<CommsMessage> {
    let foreign: Vec<&str> = LINK_FIELDS
        .iter()
        .copied()
        .filter(|f| match message.metadata.get(*f) {
            None | Some(serde_json::Value::Null) => false,
            Some(serde_json::Value::String(s)) => !s.is_empty() && !is_own_link(&message.site_id, link_domains, s),
            Some(_) => true,
        })
        .collect();
    if foreign.is_empty() {
        return None;
    }
    let mut m = message.clone();
    for f in foreign {
        let host = m.metadata.get(f).and_then(|v| v.as_str()).and_then(|s| Url::parse(s).ok())
            .and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        warn!(site_id = %m.site_id, message_id = %m.id, field = f, host = %host,
            "Link not on the site's own domain (https): dropped from the mail");
        m.metadata.remove(f);
    }
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sites_own_domain_and_its_subdomains_pass() {
        assert!(is_own_link("innovagent_nl", &[], "https://innovagent.nl/bestellen/order?t=abc"));
        assert!(is_own_link("innovagent_nl", &[], "https://www.innovagent.nl/"));
        assert!(is_own_link("innovagent_nl", &[], "https://INNOVAGENT.NL/x"));
        assert!(is_own_link("rodeloper_innovagent_nl", &[], "https://rodeloper.innovagent.nl/portal"));
    }

    #[test]
    fn anything_else_is_refused() {
        for u in [
            "http://innovagent.nl/order",
            "https://www.mollie.com/checkout/x",
            "https://innovagent.nl.evil.example/",
            "https://evilinnovagent.nl/",
            "https://innovagent.nl@evil.example/",
            "https://user:pw@innovagent.nl/",
            "https://innovagent.nl:8443/",
            "https://203.0.113.7/",
            "javascript:alert(1)",
            "//innovagent.nl/x",
            "not a url",
        ] {
            assert!(!is_own_link("innovagent_nl", &[], u), "{u} must be refused");
        }
        assert!(!is_own_link("rodeloper_innovagent_nl", &[], "https://rodeloper-innovagent.nl/"), "a hyphen look-alike");
        assert!(!is_own_link("rodeloper_innovagent_nl", &[], "https://innovagent.nl/"), "the parent is another site");
        assert!(!is_own_link("agentique", &[], "https://agentique/"), "an id with no dot names no domain");
    }

    #[test]
    fn a_listed_domain_counts_as_the_sites_own() {
        let listed = vec!["Ewaas.nl.".to_string()];
        assert!(is_own_link("ewaas_innovagent_nl", &listed, "https://ewaas.nl/order"));
        assert!(is_own_link("ewaas_innovagent_nl", &listed, "https://ewaas.innovagent.nl/order"));
        assert!(!is_own_link("ewaas_innovagent_nl", &["nl".to_string()], "https://evil.nl/"), "a bare suffix is not a domain");
    }

    #[test]
    fn foreign_links_are_dropped_and_own_ones_kept() {
        let mut m = CommsMessage {
            id: "m1".into(),
            message_type: "shop".into(),
            timestamp: String::new(),
            site_id: "innovagent_nl".into(),
            priority: 2,
            sender: None,
            content: crate::channels::MessageContent { subject: None, body: None, attachments: Default::default() },
            metadata: Default::default(),
            environment: "production".into(),
            dispatch: None,
            to: None,
        };
        m.metadata.insert("cta_url".into(), serde_json::json!("https://pay.evil.example/innovagent"));
        m.metadata.insert("cta_label".into(), serde_json::json!("Betaal"));
        m.metadata.insert("site_url".into(), serde_json::json!("https://innovagent.nl/"));
        m.metadata.insert("logo_url".into(), serde_json::json!({"src": "x"}));
        let out = own_links_only(&m, &[]).expect("changed");
        assert!(!out.metadata.contains_key("cta_url"));
        assert!(!out.metadata.contains_key("logo_url"));
        assert_eq!(out.metadata.get("site_url"), m.metadata.get("site_url"));
        assert_eq!(out.metadata.get("cta_label"), m.metadata.get("cta_label"));
        m.metadata.remove("cta_url");
        m.metadata.remove("logo_url");
        assert!(own_links_only(&m, &[]).is_none(), "nothing to drop leaves the message as it is");
    }
}
