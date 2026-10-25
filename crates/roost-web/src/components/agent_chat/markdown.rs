//! Safe Markdown rendering for assistant text. Raw HTML from the model becomes
//! text, only web-safe link schemes survive, and fenced code gets the chat's
//! code-block chrome (language label and a Copy button the transcript handles
//! by delegation) and `syntax_lite` colours for a known language. The only
//! markup emitted is markup this file writes.

use crate::components::ui::{ButtonSize, ButtonVariant, button_class};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd, html};

/// The attribute the transcript's click delegation looks for on a Copy button.
pub const COPY_CODE_ATTRIBUTE: &str = "data-copy-code";

/// Convert chat Markdown to HTML while treating host-provided content as untrusted.
pub fn markdown_to_safe_html(text: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    // The open fence: whether its language is highlighted, and its text so far.
    let mut open_code: Option<(bool, String)> = None;
    let events = Parser::new_ext(text, options).filter_map(move |event| match event {
        Event::Text(code) if open_code.is_some() => {
            if let Some((_, buffer)) = open_code.as_mut() {
                buffer.push_str(&code);
            }
            None
        }
        // The model's HTML is shown, never interpreted.
        Event::Html(raw) | Event::InlineHtml(raw) => Some(Event::Text(raw)),
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
            Some(Event::Start(Tag::Link {
                link_type,
                dest_url: safe,
                title,
                id,
            }))
        }
        Event::Start(Tag::CodeBlock(kind)) => {
            let highlighted = match &kind {
                CodeBlockKind::Fenced(info) => highlighted_language(&safe_language(info)),
                CodeBlockKind::Indented => false,
            };
            open_code = Some((highlighted, String::new()));
            Some(Event::Html(code_block_open(&kind).into()))
        }
        Event::End(TagEnd::CodeBlock) => {
            let (highlighted, code) = open_code.take().unwrap_or_default();
            Some(Event::Html(
                format!("{}</code></pre></div>", code_html(&code, highlighted)).into(),
            ))
        }
        other => Some(other),
    });
    let mut rendered = String::new();
    html::push_html(&mut rendered, events);
    rendered
}

/// Whether a fence language is one `syntax_lite` colours.
fn highlighted_language(language: &str) -> bool {
    let extension = match language.to_ascii_lowercase().as_str() {
        "rust" => "rs".to_owned(),
        "typescript" => "ts".to_owned(),
        "javascript" => "js".to_owned(),
        "python" => "py".to_owned(),
        "golang" => "go".to_owned(),
        "shell" | "console" => "sh".to_owned(),
        other => other.to_owned(),
    };
    crate::syntax_lite::should_highlight(&extension)
}

/// A code block's escaped text, one span per coloured token when highlighted.
fn code_html(code: &str, highlighted: bool) -> String {
    if !highlighted {
        return escape_html(code);
    }
    let mut html = String::with_capacity(code.len() * 2);
    let lines: Vec<&str> = code.split('\n').collect();
    let token_lines = crate::syntax_lite::tokenize_lines(lines.iter().copied());
    for (index, tokens) in token_lines.iter().enumerate() {
        if index > 0 {
            html.push('\n');
        }
        for token in tokens {
            let text = escape_html(&token.text);
            match token.kind {
                crate::syntax_lite::TokenKind::Plain => html.push_str(&text),
                kind => {
                    html.push_str("<span class=\"");
                    html.push_str(token_class(kind));
                    html.push_str("\">");
                    html.push_str(&text);
                    html.push_str("</span>");
                }
            }
        }
    }
    html
}

/// The colour of a `syntax_lite` token, from the theme's syntax palette.
fn token_class(kind: crate::syntax_lite::TokenKind) -> &'static str {
    use crate::syntax_lite::TokenKind;
    match kind {
        TokenKind::Keyword => "text-[var(--syntax-keyword)]",
        TokenKind::String => "text-[var(--syntax-string)]",
        TokenKind::Number => "text-[var(--syntax-number)]",
        TokenKind::Comment => "italic text-[var(--syntax-comment)]",
        TokenKind::Plain => "",
    }
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            other => escaped.push(other),
        }
    }
    escaped
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
    // The sanitized markup cannot mount Dioxus primitives, so it uses the
    // registry Card and Ghost button recipes directly.
    let copy_class = button_class(ButtonVariant::Ghost, ButtonSize::Xs, None);
    format!(
        "<div data-code-block class=\"my-4 overflow-hidden bg-card text-card-foreground flex flex-col rounded-xl border shadow-sm py-6 gap-4\">\
         <div class=\"flex items-center justify-between gap-2 px-6\">\
         <span class=\"font-mono text-xs text-muted-foreground\">{label}</span>\
         <button type=\"button\" class=\"{copy_class}\" {COPY_CODE_ATTRIBUTE}>Copy</button>\
         </div><pre class=\"m-0 overflow-x-auto px-6 pb-6\"><code>"
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
        assert!(html.contains(">rust</span>"), "{html}");
        assert!(html.contains("data-copy-code"), "{html}");
        assert!(html.contains("&lt;b&gt;"), "{html}");
        assert!(!html.contains("<b>"), "{html}");
    }

    #[test]
    fn known_languages_get_token_colours_and_unknown_ones_stay_plain() {
        let html = markdown_to_safe_html("```rust\nlet x = \"<b>\"; // done\n```");
        assert!(
            html.contains("<span class=\"text-[var(--syntax-keyword)]\">let</span>"),
            "{html}"
        );
        assert!(
            html.contains(
                "<span class=\"text-[var(--syntax-string)]\">&quot;&lt;b&gt;&quot;</span>"
            ),
            "{html}"
        );
        assert!(html.contains("text-[var(--syntax-comment)]"), "{html}");
        let plain = markdown_to_safe_html("```text\nlet x = 1\n```");
        let plain_code = plain.split("<code>").nth(1).unwrap_or_default();
        assert!(!plain_code.contains("<span"), "{plain}");
    }

    #[test]
    fn a_hostile_fence_language_cannot_inject_markup() {
        let html = markdown_to_safe_html("```\"><img src=x onerror=alert(1)>\nbody\n```");
        assert!(!html.contains("<img"), "{html}");
        assert!(!html.contains("onerror=alert(1)>"), "{html}");
    }

    #[test]
    fn an_inline_svg_keeps_its_source_and_plain_text_caption() {
        let source = "data:image/svg+xml;utf8,%3Csvg%20xmlns=%22http://www.w3.org/2000/svg%22%20width=%228%22%20height=%226%22%3E%3C/svg%3E";
        let html = markdown_to_safe_html(&format!(
            "![A **small** `diagram`]({source} \"Diagram title\")"
        ));
        assert!(html.contains(&format!("src=\"{source}\"")), "{html}");
        assert!(html.contains("alt=\"A small diagram\""), "{html}");
        assert!(html.contains("title=\"Diagram title\""), "{html}");
        assert!(!html.contains("<svg"), "{html}");
    }
}
