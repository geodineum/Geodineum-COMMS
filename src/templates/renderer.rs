//! Template renderer using Tera

use parking_lot::RwLock;
use tera::{Context, Tera};
use tracing::{debug, info, warn};

use crate::channels::RenderedContent;
use crate::error::{CommsError, Result};

/// Template renderer that manages Tera templates
pub struct TemplateRenderer {
    tera: RwLock<Tera>,
    template_dir: String,
}

impl TemplateRenderer {
    /// Create a new template renderer from a directory
    pub fn new(template_dir: &str) -> Result<Self> {
        let glob_pattern = format!("{}/**/*", template_dir);

        let mut tera = match Tera::new(&glob_pattern) {
            Ok(t) => {
                info!(
                    template_dir = %template_dir,
                    template_count = t.get_template_names().count(),
                    "Loaded templates"
                );
                t
            }
            Err(e) => {
                // If directory doesn't exist or is empty, create empty Tera instance
                warn!(
                    template_dir = %template_dir,
                    error = %e,
                    "Could not load templates, using empty template set"
                );
                Tera::default()
            }
        };

        tera.register_filter("tg_escape", crate::templates::tg_escape::tg_escape_filter);

        Ok(Self {
            tera: RwLock::new(tera),
            template_dir: template_dir.to_string(),
        })
    }

    /// Create a template renderer with built-in default templates
    pub fn with_defaults() -> Result<Self> {
        let mut tera = Tera::default();
        tera.register_filter("tg_escape", crate::templates::tg_escape::tg_escape_filter);

        // Register default email templates
        tera.add_raw_template(
            "email/contact.html",
            include_str!("../../config/templates/email/contact.html"),
        )
        .ok();

        tera.add_raw_template(
            "email/alert.html",
            include_str!("../../config/templates/email/alert.html"),
        )
        .ok();

        // Register default telegram templates
        tera.add_raw_template(
            "telegram/contact.md",
            include_str!("../../config/templates/telegram/contact.md"),
        )
        .ok();

        tera.add_raw_template(
            "telegram/alert.md",
            include_str!("../../config/templates/telegram/alert.md"),
        )
        .ok();

        // Register default SMS templates
        tera.add_raw_template(
            "sms/contact.txt",
            include_str!("../../config/templates/sms/contact.txt"),
        )
        .ok();

        tera.add_raw_template(
            "sms/alert.txt",
            include_str!("../../config/templates/sms/alert.txt"),
        )
        .ok();

        Ok(Self {
            tera: RwLock::new(tera),
            template_dir: String::new(),
        })
    }

    /// Render a template with the given context
    pub async fn render(&self, template_name: &str, context: &Context) -> Result<RenderedContent> {
        let tera = self.tera.read();

        // Try exact match first
        let full_name = if template_name.contains('.') {
            template_name.to_string()
        } else {
            // Try common extensions
            let extensions = ["html", "md", "txt"];
            extensions
                .iter()
                .map(|ext| format!("{}.{}", template_name, ext))
                .find(|name| tera.get_template_names().any(|t| t == name))
                .unwrap_or_else(|| format!("{}.html", template_name))
        };

        let rendered = tera.render(&full_name, context).map_err(|e| {
            debug!(
                template = %full_name,
                error = %e,
                "Template render failed"
            );
            CommsError::Template(e)
        })?;

        // Parse rendered content based on extension
        let extension = full_name
            .rsplit('.')
            .next()
            .unwrap_or("txt");

        match extension {
            "html" => {
                // For HTML templates, extract subject from <!-- subject: ... --> comment
                let (subject, body) = Self::extract_subject_from_html(&rendered);
                Ok(RenderedContent::plain(Self::strip_html(&body))
                    .with_subject(subject.unwrap_or_else(|| "Notification".to_string()))
                    .with_html(body))
            }
            "md" => {
                // For Markdown, first line is subject if it starts with #
                let (subject, body) = Self::extract_subject_from_markdown(&rendered);
                Ok(RenderedContent::plain(body).with_subject(
                    subject.unwrap_or_else(|| "Notification".to_string()),
                ))
            }
            _ => {
                // Plain text - first line is subject
                let mut lines = rendered.lines();
                let subject = lines.next().unwrap_or("Notification").to_string();
                let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
                Ok(RenderedContent::plain(body).with_subject(subject))
            }
        }
    }

    /// Reload templates from disk
    pub fn reload(&self) -> Result<()> {
        if self.template_dir.is_empty() {
            return Ok(());
        }

        let glob_pattern = format!("{}/**/*", self.template_dir);
        let new_tera = Tera::new(&glob_pattern)?;

        let mut tera = self.tera.write();
        *tera = new_tera;

        info!(
            template_count = tera.get_template_names().count(),
            "Reloaded templates"
        );

        Ok(())
    }

    /// Add or update a template
    pub fn add_template(&self, name: &str, content: &str) -> Result<()> {
        let mut tera = self.tera.write();
        tera.add_raw_template(name, content)?;
        Ok(())
    }

    /// List all template names
    pub fn list_templates(&self) -> Vec<String> {
        let tera = self.tera.read();
        tera.get_template_names().map(|s| s.to_string()).collect()
    }

    /// Check if a template exists
    pub fn has_template(&self, name: &str) -> bool {
        let tera = self.tera.read();
        let exists = tera.get_template_names().any(|t| t == name);
        exists
    }

    // Helper: extract subject from HTML comment
    fn extract_subject_from_html(html: &str) -> (Option<String>, String) {
        // Look for <!-- subject: ... --> at the start
        if html.trim().starts_with("<!--") {
            if let Some(end) = html.find("-->") {
                let comment = &html[4..end].trim();
                if let Some(subject_part) = comment.strip_prefix("subject:") {
                    let subject = subject_part.trim().to_string();
                    let body = html[end + 3..].trim().to_string();
                    return (Some(subject), body);
                }
            }
        }
        (None, html.to_string())
    }

    // Helper: extract subject from Markdown (first # heading)
    fn extract_subject_from_markdown(md: &str) -> (Option<String>, String) {
        let mut lines = md.lines();
        if let Some(first_line) = lines.next() {
            if first_line.starts_with('#') {
                let subject = first_line.trim_start_matches('#').trim().to_string();
                let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
                return (Some(subject), body);
            }
        }
        (None, md.to_string())
    }

    // Helper: strip HTML tags (basic implementation)
    /// The text/plain part of an HTML mail: no head or style contents, a line break for <br> and
    /// at the end of each block, entities decoded (Tera escapes "/" in links as &#x2F;).
    pub fn strip_html(html: &str) -> String {
        let lower = html.to_ascii_lowercase();
        let mut result = String::new();
        let mut i = 0;
        let bytes = html.as_bytes();
        while i < bytes.len() {
            if bytes[i] == b'<' {
                let end = match html[i..].find('>') {
                    Some(e) => i + e,
                    None => break,
                };
                let tag = &lower[i + 1..end];
                let name: String = tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                if !tag.starts_with('/') && (name == "head" || name == "style" || name == "script" || name == "title") {
                    let close = format!("</{}", name);
                    i = match lower[end..].find(&close) {
                        Some(c) => end + c + lower[end + c..].find('>').map(|g| g + 1).unwrap_or(0),
                        None => bytes.len(),
                    };
                    continue;
                }
                if name == "br" || (tag.starts_with('/') && matches!(name.as_str(), "p" | "div" | "tr" | "h1" | "h2" | "h3" | "li" | "table")) {
                    result.push('\n');
                }
                i = end + 1;
                continue;
            }
            let ch = html[i..].chars().next().unwrap_or(' ');
            result.push(ch);
            i += ch.len_utf8();
        }
        // Trimmed lines, at most one empty line in a row.
        let mut out: Vec<&str> = Vec::new();
        let decoded = Self::decode_entities(&result);
        for line in decoded.lines().map(str::trim) {
            if line.is_empty() && out.last().map_or(true, |l| l.is_empty()) {
                continue;
            }
            out.push(line);
        }
        out.join("\n").trim().to_string()
    }

    fn decode_entities(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(amp) = rest.find('&') {
            out.push_str(&rest[..amp]);
            let tail = &rest[amp..];
            let semi = tail.find(';').filter(|&p| p <= 10);
            let decoded = semi.and_then(|p| {
                let ent = &tail[1..p];
                let ch = match ent {
                    "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'), "quot" => Some('"'), "apos" => Some('\''), "nbsp" => Some(' '),
                    _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
                    _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
                    _ => None,
                };
                ch.map(|c| (c, p + 1))
            });
            match decoded {
                Some((c, len)) => { out.push(c); rest = &tail[len..]; }
                None => { out.push('&'); rest = &tail[1..]; }
            }
        }
        out.push_str(rest);
        out
    }
}

#[cfg(test)]
mod strip_tests {
    use super::*;

    #[test]
    fn text_part_drops_head_and_style_keeps_breaks_decodes_entities() {
        let html = "<html><head><title>T</title><style>body{color:red}</style></head><body><p>Hallo,</p><p>Klik &lt;hier&gt;<br>https:&#x2F;&#x2F;x.nl&#x2F;demo &amp; meer</p><p>a&nbsp;b &unknown; &#233;</p></body></html>";
        let text = TemplateRenderer::strip_html(html);
        assert_eq!(text, "Hallo,\nKlik <hier>\nhttps://x.nl/demo & meer\na b &unknown; \u{e9}");
    }
}

#[cfg(test)]
mod tg_template_tests {
    use super::*;

    /// Render the REAL alert template with the real grants-CLI subject shape.
    /// This is the exact input that failed with 400 "can't parse entities"
    /// for every alert-type telegram notification since the template shipped:
    /// the template interpolated raw while parse_mode was MarkdownV2, and the
    /// escaping lived only in the channel's fallback branch.
    #[tokio::test]
    async fn alert_template_output_is_valid_markdown_v2() {
        let r = TemplateRenderer::with_defaults().unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("content", &serde_json::json!({
            "subject": "[geodineum] grant request gr-1785582550-11706: buttontest",
            "body": "Service 'buttontest' requests ValKey access:\n\n  patterns: {buttontest}:demo:*\n  timeout:  auto-DENY after 1h",
        }));
        ctx.insert("message", &serde_json::json!({"priority": 2}));
        ctx.insert("site_id", "geodine");
        ctx.insert("timestamp", "2026-08-01T11:15:17+00:00");
        ctx.insert("metadata", &serde_json::json!({}));
        ctx.insert("sender", &serde_json::json!(null));

        let out = r.render("telegram/alert.md", &ctx).await.unwrap();
        let body = &out.body;

        assert!(body.contains("\\[geodineum\\]"), "brackets unescaped: {}", body);
        assert!(body.contains("2026\\-08\\-01"), "timestamp unescaped: {}", body);
        assert!(!body.contains("\n# "), "markdown heading is not MarkdownV2: {}", body);
        // Every '-' '.' '{' '}' outside a formatting entity must carry a
        // backslash. Spot-check the pattern line from the body.
        assert!(body.contains("\\{buttontest\\}"), "braces unescaped: {}", body);
        // The label is Service, not Site — site_id is a legacy wire name the
        // operator-facing text must not leak. And the brand header must carry
        // the logo link that Telegram renders as the preview card.
        assert!(body.contains("*Service:*"), "label must be Service: {}", body);
        assert!(body.contains("cropped-Geodineum_Logo.png"), "brand link missing: {}", body);
    }
}
