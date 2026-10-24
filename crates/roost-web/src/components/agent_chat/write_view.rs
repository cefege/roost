//! The write box body: OMP's numbered preview of the content a `write` call
//! creates, the head window and its `+N more lines` hint. The content is the
//! call's own argument and is drawn as text, never as markup.

use dioxus::prelude::*;

use super::output_lines::window;
use crate::components::ui::{Button, ButtonSize, ButtonVariant, Surface};

/// Lines of written content shown before folding, matching OMP's write preview.
pub(super) const WRITE_PREVIEW_LINES: usize = 6;

/// The numbered head of written content: `(line number, text)` pairs, the count
/// of hidden lines, the whole line count, and the gutter's digit width.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct WritePreview<'a> {
    pub(super) lines: Vec<(usize, &'a str)>,
    pub(super) hidden: usize,
    pub(super) total: usize,
    pub(super) gutter: usize,
}

pub(super) fn write_preview(content: &str, open: bool) -> WritePreview<'_> {
    let all_lines: Vec<&str> = content.lines().collect();
    let (shown, hidden) = window(&all_lines, WRITE_PREVIEW_LINES, true, open);
    let lines = shown
        .iter()
        .enumerate()
        .map(|(offset, line)| (offset + 1, *line))
        .collect();
    let gutter = all_lines.len().to_string().len().max(3);
    WritePreview {
        lines,
        hidden,
        total: all_lines.len(),
        gutter,
    }
}

#[component]
pub fn WriteBody(content: String, open: bool, on_expand: EventHandler<bool>) -> Element {
    let preview = write_preview(&content, open);
    let hint = format!("+{} more lines", preview.hidden);
    let can_collapse = open && preview.total > WRITE_PREVIEW_LINES;
    let gutter_style = format!("--write-gutter: {};", preview.gutter);
    rsx! {
        Surface { class: "rounded-md bg-terminal py-2 text-terminal-foreground",
            pre { class: "m-0 overflow-x-auto font-mono text-xs", style: gutter_style,
                for (number, line) in preview.lines.iter() {
                    div { class: "flex whitespace-pre", key: "{number}",
                        span { class: "min-w-[calc(var(--write-gutter)*1ch)] select-none pr-3 text-right text-muted-foreground", "{number}" }
                        span { class: "whitespace-pre-wrap break-words", "{line}" }
                    }
                }
            }
            if preview.hidden > 0 {
                p { class: "m-0 px-3 pt-1 text-xs text-muted-foreground", "{hint}" }
            }
        }
        if preview.hidden > 0 {
            Button { variant: ButtonVariant::Link, size: ButtonSize::Sm, class: "self-start",
                onclick: move |_| on_expand.call(true), "Show all" }
        }
        if can_collapse {
            Button { variant: ButtonVariant::Link, size: ButtonSize::Sm, class: "self-start",
                onclick: move |_| on_expand.call(false), "Collapse" }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{WRITE_PREVIEW_LINES, write_preview};

    #[test]
    fn the_head_is_numbered_and_the_rest_is_counted() {
        let content = "line\n".repeat(10);
        let preview = write_preview(&content, false);
        assert_eq!(preview.lines.len(), WRITE_PREVIEW_LINES);
        assert_eq!(preview.lines.first(), Some(&(1, "line")));
        assert_eq!(preview.lines.last(), Some(&(6, "line")));
        assert_eq!(preview.hidden, 4);
        assert_eq!(preview.total, 10);
        assert_eq!(preview.gutter, 3);
    }

    #[test]
    fn the_gutter_widens_with_the_line_count() {
        let content = "x\n".repeat(1234);
        assert_eq!(write_preview(&content, false).gutter, 4);
    }

    #[test]
    fn an_open_preview_shows_every_line() {
        let content = "x\n".repeat(9);
        let preview = write_preview(&content, true);
        assert_eq!(preview.lines.len(), 9);
        assert_eq!(preview.hidden, 0);
    }
}
