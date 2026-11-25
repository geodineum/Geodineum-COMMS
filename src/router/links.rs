//! A mail's button, footer link and logo come from the message's metadata, which more than the site can
//! write to its stream. They are kept only when they point at the sending site itself. A message body in HTML
//! (`metadata.html`, the guest template) is cleaned to a short list of text tags and safe inline styles, its links
//! held to the same rule, before any template sees it.

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;

use reqwest::Url;
use tracing::warn;

use crate::channels::CommsMessage;

/// Metadata fields a template turns into a link or an image.
pub const LINK_FIELDS: &[&str] = &["cta_url", "site_url", "logo_url"];

/// The other domains a site's mail may link to, one line per site: `<site_id> <domain> [<domain>...]`.
/// Root writes it (`geodineum comms links`); a site's own settings cannot add to it, because the site writes those.
pub const LINK_DOMAINS_FILE: &str = "/etc/geodineum/mail/link-domains";

/// The domains `body` lists for `site_id`; `#` starts a comment, a site may have several lines.
pub fn listed_domains(body: &str, site_id: &str) -> Vec<String> {
    body.lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            (words.next() == Some(site_id)).then(|| words.map(norm).collect::<Vec<_>>())
        })
        .flatten()
        .collect()
}

/// The domains the operators' file lists for `site_id`; none when the file is absent or unreadable.
pub fn operator_link_domains(file: &Path, site_id: &str) -> Vec<String> {
    std::fs::read_to_string(file).map(|b| listed_domains(&b, site_id)).unwrap_or_default()
}

/// The domain a site id names: `innovagent_nl` is innovagent.nl, `rodeloper_innovagent_nl` is
/// rodeloper.innovagent.nl. Only `_` maps back to `.`, so a hyphenated look-alike
/// (rodeloper-innovagent.nl) never matches; a site whose domain has a hyphen needs an operator to list it.
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

/// The metadata field a template may render as HTML: only ever after [`clean_html`].
pub const HTML_FIELD: &str = "html";

const HTML_TAGS: &[&str] = &["p", "br", "strong", "b", "em", "i", "u", "a", "ul", "ol", "li", "h2", "h3", "blockquote", "hr", "span", "div"];
const STYLE_PROPERTIES: &[&str] = &[
    "color", "background-color", "font-size", "font-weight", "font-style", "font-family", "line-height", "letter-spacing", "text-transform",
    "text-decoration", "text-align", "margin", "margin-top", "margin-right", "margin-bottom", "margin-left", "padding", "padding-top",
    "padding-right", "padding-bottom", "padding-left", "border", "border-top", "border-bottom", "border-left",
];

/// An HTML body cut down to text markup: the tags above, inline styles of the properties above only (so no
/// url(), no positioning, no images), and links that are https to the site's own domains or mailto one of them.
/// Scripts, styles, forms, images and every other tag or attribute are removed; their text stays.
pub fn clean_html(site_id: &str, link_domains: &[String], html: &str) -> String {
    let site = site_id.to_string();
    let domains = link_domains.to_vec();
    ammonia::Builder::default()
        .tags(HTML_TAGS.iter().copied().collect::<HashSet<_>>())
        .generic_attributes(["style"].into_iter().collect::<HashSet<_>>())
        .tag_attributes(std::iter::once(("a", ["href"].into_iter().collect::<HashSet<_>>())).collect())
        .filter_style_properties(STYLE_PROPERTIES.iter().copied().collect::<HashSet<_>>())
        .url_schemes(["https", "mailto"].into_iter().collect::<HashSet<_>>())
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer"))
        .attribute_filter(move |_, attr, value| {
            if attr != "href" {
                return Some(Cow::Owned(value.to_string()));
            }
            let own = match value.trim().strip_prefix("mailto:") {
                Some(addr) => addr.rsplit_once('@').is_some_and(|(local, host)| {
                    !local.is_empty() && !host.contains(['?', '/', ':']) && is_own_link(&site, &domains, &format!("https://{host}/"))
                }),
                None => is_own_link(&site, &domains, value),
            };
            own.then(|| Cow::Owned(value.to_string()))
        })
        .clean(html)
        .to_string()
}

/// The message with every link field that does not point at its own site removed (the template then
/// renders no button, no footer link, no logo), and an HTML body cleaned, each change logged.
pub fn own_links_only(message: &CommsMessage, link_domains: &[String]) -> Option<CommsMessage> {
    let html = match message.metadata.get(HTML_FIELD) {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => {
            let clean = clean_html(&message.site_id, link_domains, s);
            (clean != *s).then_some(clean)
        }
        Some(_) => Some(String::new()),
    };
    let foreign: Vec<&str> = LINK_FIELDS
        .iter()
        .copied()
        .filter(|f| match message.metadata.get(*f) {
            None | Some(serde_json::Value::Null) => false,
            Some(serde_json::Value::String(s)) => !s.is_empty() && !is_own_link(&message.site_id, link_domains, s),
            Some(_) => true,
        })
        .collect();
    if foreign.is_empty() && html.is_none() {
        return None;
    }
    let mut m = message.clone();
    if let Some(clean) = html {
        warn!(site_id = %m.site_id, message_id = %m.id, "HTML body held markup, styles or links not allowed: cleaned before the template");
        m.metadata.insert(HTML_FIELD.to_string(), serde_json::Value::String(clean));
    }
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
    fn the_operators_file_lists_domains_per_site() {
        let body = "# site domains\nalpha_nl Shop-Alpha.example. other.example # a comment\nbeta_nl beta.example\nalpha_nl third.example\n\n#alpha_nl hidden.example\nalpha_nl_x x.example\n";
        assert_eq!(listed_domains(body, "alpha_nl"), vec!["shop-alpha.example", "other.example", "third.example"]);
        assert_eq!(listed_domains(body, "beta_nl"), vec!["beta.example"]);
        assert!(listed_domains(body, "gamma_nl").is_empty());
        assert!(listed_domains(body, "alpha").is_empty(), "a prefix of a site id is another site");
        assert!(operator_link_domains(Path::new("/nonexistent/link-domains"), "alpha_nl").is_empty());
    }

    #[test]
    fn an_html_body_keeps_text_markup_and_own_links_only() {
        let dirty = concat!(
            "<p style=\"margin:0 0 16px;color:#1d1c1a;background:url(https://evil.example/x.png);position:fixed\" onclick=\"x()\">Beste <strong>Sanne</strong>,</p>",
            "<script>steal()</script><style>p{}</style><img src=\"https://evil.example/t.gif\">",
            "<p><a href=\"https://twentyseven.innovagent.nl/menu\" style=\"color:#8a6d3b\">menu</a> ",
            "<a href=\"https://evil.example/login\">login</a> <a href=\"javascript:alert(1)\">x</a> <a href=\"/relative\">r</a> ",
            "<a href=\"mailto:gasten@twentyseven.innovagent.nl\">mail</a> <a href=\"mailto:x@evil.example\">evil</a></p>",
            "<form action=\"https://evil.example\"><input name=\"pw\"></form><iframe src=\"https://evil.example\"></iframe>"
        );
        let clean = clean_html("twentyseven_innovagent_nl", &[], dirty);
        assert!(clean.contains("Beste <strong>Sanne</strong>,"), "{clean}");
        assert!(clean.contains("margin:0 0 16px") && clean.contains("color:#1d1c1a"), "safe styles kept: {clean}");
        for gone in ["url(", "position", "onclick", "<script", "steal()", "<style", "<img", "evil.example", "javascript:", "/relative", "<form", "<input", "<iframe"] {
            assert!(!clean.contains(gone), "{gone} must be gone: {clean}");
        }
        assert!(clean.contains("href=\"https://twentyseven.innovagent.nl/menu\""), "{clean}");
        assert!(clean.contains("href=\"mailto:gasten@twentyseven.innovagent.nl\""), "{clean}");
        assert!(clean.contains(">login</a>") || clean.contains("login"), "a refused link keeps its text");
        assert!(clean.contains("rel=\"noopener noreferrer\""));
    }

    #[test]
    fn a_message_with_an_unclean_html_body_is_cleaned_and_a_clean_one_left() {
        let mut m = CommsMessage {
            id: "m2".into(),
            message_type: "guest".into(),
            timestamp: String::new(),
            site_id: "twentyseven_innovagent_nl".into(),
            priority: 2,
            sender: None,
            content: crate::channels::MessageContent { subject: None, body: None, attachments: Default::default() },
            metadata: Default::default(),
            environment: "production".into(),
            dispatch: None,
            to: None,
        };
        m.metadata.insert("html".into(), serde_json::json!("<p>Dank</p><script>x()</script>"));
        let out = own_links_only(&m, &[]).expect("changed");
        assert_eq!(out.metadata.get("html"), Some(&serde_json::json!("<p>Dank</p>")));
        m.metadata.insert("html".into(), serde_json::json!("<p>Dank</p>"));
        assert!(own_links_only(&m, &[]).is_none(), "a clean body is left as it is");
        m.metadata.insert("html".into(), serde_json::json!({"not": "text"}));
        assert_eq!(own_links_only(&m, &[]).expect("changed").metadata.get("html"), Some(&serde_json::json!("")));
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
