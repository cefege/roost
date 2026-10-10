//! Safe Markdown rendering for assistant text. Raw HTML from the model becomes
//! text, only web-safe link schemes survive, and fenced code gets the chat's
//! code-block chrome (language label and a Copy button the transcript handles
//! by delegation). The only markup emitted is markup this file writes.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd, html};

/// The attribute the transcript's click delegation looks for on a Copy button.
pub const COPY_CODE_ATTRIBUTE: &str = "data-copy-code";

/// Convert chat Markdown to HTML while treating host-provided content as untrusted.
pub fn markdown_to_safe_html(text: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(text, options).map(|event| match event {
        // The model's HTML is shown, never interpreted.
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
        Event::Start(Tag::CodeBlock(kind)) => Event::Html(code_block_open(&kind).into()),
        Event::End(TagEnd::CodeBlock) => Event::Html("</code></pre></div>".into()),
        other => other,
    });
    let mut rendered = String::new();
    html::push_html(&mut rendered, events);
    rendered
}

fn code_block_open(kind: &CodeBlockKind<'_>) -> String {
    let language = match kind {
        CodeBlockKind::Fenced(info) => safe_language(info),
        CodeBlockKind::Indented => String::new(),
    };
    let label = if language.is_empty() {
        "code"
    } else {
        language.as_str()
    };
    // The block lives in sanitized `innerHTML`, where no component can mount,
    // so the button takes the `Button` primitive's own classes.
    let button_class = crate::components::md::button::button_class(
        crate::components::md::ButtonVariant::Ghost,
        crate::components::md::ButtonSize::Xs,
        Some("agent-chat__code-copy"),
    );
    format!(
        "<div class=\"agent-chat__code\"><div class=\"agent-chat__code-bar\">\
         <span class=\"agent-chat__code-language\">{label}</span>\
         <button type=\"button\" class=\"{button_class}\" {COPY_CODE_ATTRIBUTE}>Copy</button>\
         </div><pre><code>"
    )
}

/// The fence's language word, reduced to characters that cannot leave the
/// attribute-free text node it is written into.
fn safe_language(info: &str) -> String {
    info.split_whitespace()
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || "+#.-_".contains(*character))
        .take(24)
        .collect()
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

    #[test]
    fn fenced_code_gets_a_labelled_copyable_block_with_escaped_contents() {
        let html = markdown_to_safe_html("```rust\nlet x = \"<b>\";\n```");
        assert!(html.contains("agent-chat__code-language\">rust<"), "{html}");
        assert!(html.contains("data-copy-code"), "{html}");
        assert!(html.contains("&lt;b&gt;"), "{html}");
        assert!(!html.contains("<b>"), "{html}");
    }

    #[test]
    fn a_hostile_fence_language_cannot_inject_markup() {
        let html = markdown_to_safe_html("```\"><img src=x onerror=alert(1)>\nbody\n```");
        assert!(!html.contains("<img"), "{html}");
        assert!(!html.contains("onerror=alert(1)>"), "{html}");
    }
}
