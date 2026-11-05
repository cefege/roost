//! Styled native text runs, safe links, Nerd icon runs and Markdown marks.
//! `s` is a compound token list, so `dim mono bold` applies every style. Theme
//! colour names map onto Roost roles; unknown tokens never reach the DOM.
use super::super::super::links::NativeLink;
use super::super::{render_icon, text_value};
use dioxus::prelude::*;
use serde_json::Value;

/// One rendered run's style, accumulated across every token in `s`.
#[derive(Default)]
struct RunStyle {
    classes: Vec<&'static str>,
    hidden: bool,
    icon: bool,
    href: Option<String>,
}

impl RunStyle {
    fn push(&mut self, class: &'static str) {
        if !self.classes.contains(&class) {
            self.classes.push(class);
        }
    }

    fn classes(&self) -> String {
        let mut classes = self
            .classes
            .iter()
            .filter(|class| {
                let class = **class;
                !(self.icon && class == "font-mono")
            })
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        if self.icon {
            if !classes.is_empty() {
                classes.push(' ');
            }
            classes.push_str("[font-family:var(--term-font-family)] leading-none");
        }
        classes
    }
}

/// One styled native string or span list.
pub(super) fn render(value: Option<&Value>, icon: &str, extra_class: &str) -> Element {
    let Some(value) = value else {
        return rsx! { span {} };
    };
    let runs = value.as_array();
    let wrapper = format!("min-w-0 text-sm {extra_class}");
    let Some(runs) = runs else {
        return rsx! {
            span { class: "{wrapper}",
                if !icon.is_empty() {
                    span { class: "mr-1 inline-block align-middle text-muted-foreground", {render_icon(icon, "size-3.5")} }
                }
                span { "{text_value(value)}" }
            }
        };
    };
    rsx! {
        span { class: "{wrapper}",
            if !icon.is_empty() {
                span { class: "mr-1 inline-block align-middle text-muted-foreground", {render_icon(icon, "size-3.5")} }
            }
            for run in runs {
                { run_element(run, "") }
            }
        }
    }
}

fn run_element(run: &Value, extra_class: &str) -> Element {
    let text = run.get("t").and_then(Value::as_str).unwrap_or_default();
    let tokens = run.get("s").and_then(Value::as_str);
    let mut style = RunStyle {
        href: run.get("href").and_then(Value::as_str).map(str::to_owned),
        ..RunStyle::default()
    };
    apply_tokens(&mut style, tokens);
    if style.hidden || text.is_empty() {
        return rsx! {};
    }
    let mut classes = style.classes();
    if let Some(effect) = run.get("fx").and_then(Value::as_str) {
        match effect {
            "shimmer" => classes.push_str(" shimmer"),
            "pulse" => classes.push_str(" motion-safe:animate-pulse"),
            _ => {}
        }
    }
    if !extra_class.is_empty() && !classes.is_empty() {
        classes.push(' ');
        classes.push_str(extra_class);
    }
    if let Some(href) = style.href.filter(|href| !href.is_empty()) {
        rsx! { NativeLink { href, class: Some(classes), "{text}" } }
    } else {
        rsx! { span { class: "{classes}", "{text}" } }
    }
}

fn apply_tokens(style: &mut RunStyle, styles: Option<&str>) {
    for token in styles.unwrap_or_default().split_whitespace() {
        match token {
            "muted" | "dim" | "toolDiffContext" => style.push("text-muted-foreground"),
            "strong" | "bold" => style.push("font-semibold"),
            "em" | "italic" => style.push("italic"),
            "accent" | "link" | "mdLink" | "mdLinkUrl" | "borderAccent" => {
                style.push("text-primary")
            }
            "success" | "toolDiffAdded" | "statusLineGitClean" => style.push("text-success-dark"),
            "warning" => style.push("text-warning-dark"),
            "error" | "toolDiffRemoved" => style.push("text-destructive"),
            "info" => style.push("text-info-dark"),
            "code" | "mono" | "path" | "key" | "num" | "mdCode" | "statusLinePath" => {
                style.push("font-mono")
            }
            "ins" => {
                style.push("bg-success-light");
                style.push("text-success-dark");
            }
            "del" => {
                style.push("bg-destructive/10");
                style.push("text-destructive");
                style.push("line-through");
            }
            "mark" => {
                style.push("rounded-sm");
                style.push("bg-accent");
                style.push("text-accent-foreground");
            }
            "underline" => style.push("underline"),
            "typo" => {
                style.push("underline");
                style.push("decoration-destructive");
                style.push("decoration-wavy");
            }
            "icon" => style.icon = true,
            "hide" => style.hidden = true,
            "thinkingText" | "mdQuote" | "borderMuted" | "statusLineSep" => {
                style.push("text-muted-foreground");
                style.push("italic");
            }
            "toolTitle" | "mdHeading" | "statusLineModel" | "customMessageLabel" => {
                style.push("font-semibold")
            }
            "toolOutput"
            | "assistantMessageText"
            | "userMessageText"
            | "customMessageText"
            | "text" => style.push("text-foreground"),
            "mdHr" | "border" => style.push("text-border"),
            "mdCodeBlock" => style.push("font-mono"),
            "mdListBullet" => style.push("text-primary"),
            "syntaxComment" => style.push("text-[var(--syntax-comment)]"),
            "syntaxKeyword" => style.push("text-[var(--syntax-keyword)]"),
            "syntaxFunction" => style.push("text-[var(--syntax-function)]"),
            "syntaxVariable" => style.push("text-[var(--syntax-variable)]"),
            "syntaxString" => style.push("text-[var(--syntax-string)]"),
            "syntaxNumber" => style.push("text-[var(--syntax-number)]"),
            "syntaxType" => style.push("text-[var(--syntax-type)]"),
            "syntaxOperator" => style.push("text-[var(--syntax-operator)]"),
            "syntaxPunctuation" => style.push("text-[var(--syntax-punctuation)]"),
            "statusLineOutput" | "statusLineContext" | "statusLineSpend" | "statusLineCost" => {
                style.push("font-mono")
            }
            "bashMode" => {
                style.push("font-mono");
                style.push("text-success-dark");
            }
            "pythonMode" => {
                style.push("font-mono");
                style.push("text-info-dark");
            }
            "thinkingOff" | "thinkingMinimal" | "thinkingLow" | "thinkingMedium"
            | "thinkingHigh" | "thinkingXhigh" | "thinkingMax" => {
                style.push("text-muted-foreground")
            }
            _ => {}
        }
    }
}

/// Markdown with native `marks` applied only across prose text, never inside
/// tags, code or pre. The mark's class list comes from the same token map.
pub(super) fn markdown_with_marks(source: &str, marks: Option<&Value>) -> String {
    let html = super::super::super::super::markdown::markdown_to_safe_html(source);
    let Some(marks) = marks.and_then(Value::as_array) else {
        return html;
    };
    let marks: Vec<(String, String)> = marks
        .iter()
        .filter_map(|mark| {
            let text = mark.get("t").and_then(Value::as_str)?;
            if text.is_empty() {
                return None;
            }
            let mut style = RunStyle::default();
            apply_tokens(&mut style, mark.get("s").and_then(Value::as_str));
            Some((escape_html_text(text), style.classes()))
        })
        .collect();
    if marks.is_empty() {
        return html;
    }
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0;
    let mut opaque_tags = 0_usize;
    while cursor < html.len() {
        let rest = &html[cursor..];
        if rest.starts_with('<') {
            let end = tag_end(&html, cursor);
            let tag = &html[cursor..end];
            if is_opaque_open(tag) {
                opaque_tags += 1;
            } else if is_opaque_close(tag) {
                opaque_tags = opaque_tags.saturating_sub(1);
            }
            out.push_str(tag);
            cursor = end;
        } else {
            let end = html[cursor..]
                .find('<')
                .map_or(html.len(), |offset| cursor + offset);
            let text = &html[cursor..end];
            if opaque_tags == 0 {
                apply_mark_text(&mut out, text, &marks);
            } else {
                out.push_str(text);
            }
            cursor = end;
        }
    }
    out
}

fn escape_html_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn tag_end(html: &str, start: usize) -> usize {
    let mut quote = None;
    for (offset, character) in html[start..].char_indices() {
        match (quote, character) {
            (Some(open), current) if current == open => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, '>') => return start + offset + character.len_utf8(),
            _ => {}
        }
    }
    html.len()
}

fn is_opaque_open(tag: &str) -> bool {
    tag.strip_prefix('<')
        .is_some_and(|tag| tag.starts_with("pre") || tag.starts_with("code"))
}

fn is_opaque_close(tag: &str) -> bool {
    tag.strip_prefix("</")
        .is_some_and(|tag| tag.starts_with("pre") || tag.starts_with("code"))
}

fn apply_mark_text(out: &mut String, text: &str, marks: &[(String, String)]) {
    let mut remaining = text;
    while !remaining.is_empty() {
        let next = marks
            .iter()
            .filter_map(|(mark, classes)| {
                remaining
                    .find(mark.as_str())
                    .map(|at| (at, usize::MAX - mark.len(), mark.as_str(), classes.as_str()))
            })
            .min_by_key(|(at, length, _, _)| (*at, *length));
        let Some((at, _, mark, classes)) = next else {
            out.push_str(remaining);
            break;
        };
        out.push_str(&remaining[..at]);
        if classes.is_empty() {
            out.push_str(mark);
        } else {
            out.push_str("<span class=\"");
            out.push_str(classes);
            out.push_str("\">");
            out.push_str(mark);
            out.push_str("</span>");
        }
        remaining = &remaining[at + mark.len()..];
    }
}

#[cfg(test)]
mod tests {
    use super::markdown_with_marks;

    #[test]
    fn marks_style_prose_only_and_never_open_markup() {
        let marks = serde_json::json!([
            { "t": "/skill", "s": "accent strong" },
            { "t": "<x>", "s": "code" }
        ]);
        let html = markdown_with_marks("run /skill and `code /skill` plus <x>", Some(&marks));
        assert_eq!(
            html.matches("<span class=\"text-primary font-semibold\">/skill</span>")
                .count(),
            1,
            "{html}"
        );
        assert!(!html.contains("<x>"), "{html}");
        assert!(html.contains("&lt;x&gt;"), "{html}");
    }
}
