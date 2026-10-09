//! Safe Markdown rendering for assistant text. Raw HTML is converted to text
//! and only web-safe link schemes survive before HTML serialization.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, html};

/// Convert chat Markdown to HTML while treating host-provided content as untrusted.
pub fn markdown_to_safe_html(text: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(text, options).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            let safe = if safe_link(&dest_url) {
                dest_url
            } else {
                "".into()
            };
            Event::Start(Tag::Link {
                link_type,
                dest_url: safe,
                title,
                id,
            })
        }
        Event::End(TagEnd::Link) => Event::End(TagEnd::Link),
        other => other,
    });
    let mut rendered = String::new();
    html::push_html(&mut rendered, events);
    rendered
}

fn safe_link(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
}

#[cfg(test)]
mod tests {
    use super::markdown_to_safe_html;

    #[test]
    fn raw_script_markup_is_escaped_as_text() {
        let html = markdown_to_safe_html("<script>alert(1)</script>");
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn javascript_links_are_neutralized() {
        let html = markdown_to_safe_html("[x](javascript:alert(1))");
        assert!(!html.contains("javascript:"));
    }
}
