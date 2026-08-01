//! MarkdownV2 escaping — ONE copy of the character set.
//!
//! Telegram's MarkdownV2 rejects the whole message when any special
//! character appears unescaped outside an entity. The escape existed only in
//! the telegram channel's FALLBACK rendering; the template path interpolated
//! subject/body/timestamp raw, so every alert-type telegram notification
//! failed with 400 "can't parse entities" and retried forever — silently, no
//! delivery, since the templates shipped. Registered as the Tera filter
//! `tg_escape` so templates escape at the interpolation site:
//!
//!     *Alert: {{ content.subject | tg_escape }}*

/// Everything MarkdownV2 treats as syntax. Backslash is NOT in the list —
/// prepending one per special is the escape; escaping '\\' itself would
/// double every escape the template author wrote deliberately.
pub const MARKDOWN_V2_SPECIALS: [char; 18] = [
    '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
];

pub fn escape_markdown_v2(text: &str) -> String {
    let mut result = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        if MARKDOWN_V2_SPECIALS.contains(&c) {
            result.push('\\');
        }
        result.push(c);
    }
    result
}

/// Tera filter wrapper: `{{ value | tg_escape }}`. Non-string values are
/// stringified first, so numbers and timestamps escape too.
pub fn tg_escape_filter(
    value: &tera::Value,
    _args: &std::collections::HashMap<String, tera::Value>,
) -> tera::Result<tera::Value> {
    let s = match value {
        tera::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    Ok(tera::Value::String(escape_markdown_v2(&s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact string that has been failing: the grants CLI subject.
    #[test]
    fn the_grants_subject_becomes_valid() {
        let out = escape_markdown_v2("[geodineum] grant request gr-1785582550-11706: buttontest");
        assert!(out.starts_with("\\[geodineum\\]"));
        assert!(out.contains("gr\\-1785582550\\-11706"));
    }

    #[test]
    fn timestamps_escape_their_specials() {
        assert_eq!(
            escape_markdown_v2("2026-08-01T11:15:17+00:00"),
            "2026\\-08\\-01T11:15:17\\+00:00"
        );
    }

    #[test]
    fn plain_text_passes_untouched() {
        assert_eq!(escape_markdown_v2("hello world"), "hello world");
    }
}
