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

        let tera = match Tera::new(&glob_pattern) {
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

        Ok(Self {
            tera: RwLock::new(tera),
            template_dir: template_dir.to_string(),
        })
    }

    /// Create a template renderer with built-in default templates
    pub fn with_defaults() -> Result<Self> {
        let mut tera = Tera::default();

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
    fn strip_html(html: &str) -> String {
        let mut result = String::new();
        let mut in_tag = false;

        for c in html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => result.push(c),
                _ => {}
            }
        }

        // Clean up whitespace
        result
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}
