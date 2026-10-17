//! The file's body: one row per line, the target line's marker, and the
//! per-line control that copies a link to that line. Called by
//! `file_viewer` once a read produced text; depends on `syntax_lite` for the
//! colours and on `file_viewer::state` for the colour of each token.
//!
//! Ports the `lines().length > 0` branch of
//! `apps/web/src/components/browse/FileViewerSheet.tsx`. The row is a flex line
//! of gutter, text and control, which is what makes the control reachable by
//! Tab from anywhere the reader's focus lands in the body.

use dioxus::prelude::*;

use crate::components::ui::{Button, ButtonSize, ButtonVariant, ScrollArea};
use crate::syntax_lite::Token;

use super::state::token_color;

/// The row's box, in pixels. The line-height IS the row height, which is what
/// lets the target marker be placed by arithmetic on the line number.
const ROW_HEIGHT_PX: u32 = 20;

/// How long a copied line shows its tick or cross before the row goes back to
/// `#`. v2's copy feedback window.
pub const COPY_FEEDBACK_MS: u32 = 1500;

/// What a line's copy-link control is showing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopiedLine {
    /// The line whose link was copied.
    pub line: usize,
    /// Whether the clipboard took it.
    pub ok: bool,
}

/// The scrollable body: every line, in order, with the target line marked.
#[component]
pub fn FileBody(
    lines: Vec<String>,
    tokens: Option<Vec<Vec<Token>>>,
    target_line: usize,
    copied: Option<CopiedLine>,
    on_copy: EventHandler<usize>,
) -> Element {
    let mut rows: Vec<Element> = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        let line_number = index + 1;
        rows.push(rsx! {
            FileBodyLine {
                key: "{line_number}",
                number: line_number,
                text: line.clone(),
                tokens: tokens.as_ref().and_then(|grid| grid.get(index)).cloned(),
                is_target: line_number == target_line,
                copied: copied.filter(|copied| copied.line == line_number),
                on_copy,
            }
        });
    }
    let marker_top_px =
        u32::try_from(target_line.saturating_sub(1)).unwrap_or_default() * ROW_HEIGHT_PX;
    rsx! {
        ScrollArea { class: "flex-auto min-h-0 bg-terminal rounded-sm font-mono text-sm",
            div {
                "data-testid": "file-viewer-sheet-body",
                class: "relative p-2",
                div {
                    "data-testid": "file-viewer-sheet-target-marker",
                    class: "absolute left-0 w-0.75 bg-warning",
                    style: "top: {marker_top_px}px; height: {ROW_HEIGHT_PX}px;"
                }
                {rows.into_iter()}
            }
        }
    }
}

/// One line: its number, its text, and the control that copies a link to it.
#[component]
fn FileBodyLine(
    number: usize,
    text: String,
    tokens: Option<Vec<Token>>,
    is_target: bool,
    copied: Option<CopiedLine>,
    on_copy: EventHandler<usize>,
) -> Element {
    let label = format!("Copy link to line {number}");
    rsx! {
        div {
            "data-line": "{number}",
            "data-testid": "file-viewer-sheet-line-{number}",
            class: "group flex gap-3 relative",
            style: row_style(is_target),
            span {
                "data-testid": "file-viewer-sheet-line-num-{number}",
                "data-target": is_target.then_some("true"),
                class: "min-w-9 text-right text-muted-foreground select-none shrink-0",
                "{number}"
            }
            match tokens {
                Some(tokens) => highlighted_line(&tokens),
                None => plain_line(&text),
            }
            Button {
                variant: ButtonVariant::Ghost,
                size: ButtonSize::Xs,
                class: "ml-auto shrink-0 min-h-0 any-pointer-coarse:min-h-0 px-1 text-xs opacity-0 group-hover:opacity-100 focus-visible:opacity-100 tv:opacity-100 pad:opacity-100",
                "data-testid": "file-viewer-sheet-copy-link-{number}",
                title: label.clone(),
                "aria-label": label,
                onclick: move |_| on_copy.call(number),
                span {
                    class: copy_glyph_class(copied),
                    {copy_glyph(copied)}
                }
            }
        }
    }
}

/// One line's row: the target line is tinted so the eye lands on it, and the
/// control only appears on hover so a body of two thousand lines is not two
/// thousand controls.
fn row_style(is_target: bool) -> String {
    let highlight = if is_target {
        "background-color: color-mix(in srgb, var(--ansi-bright-yellow) 10%, transparent);"
    } else {
        ""
    };
    format!("line-height: {ROW_HEIGHT_PX}px; {highlight}")
}

/// The line's text, one span per token so each run is painted in its own colour.
fn highlighted_line(tokens: &[Token]) -> Element {
    rsx! {
        span {
            class: "whitespace-pre",
            for token in tokens {
                span { style: "color: {token_color(token.kind)};", {token.text.clone()} }
            }
        }
    }
}

/// The line's text, unhighlighted.
fn plain_line(line: &str) -> Element {
    rsx! {
        span { class: "whitespace-pre text-(--syntax-plain)", {line} }
    }
}

/// What the line's control says right now: `#` until the line is copied, then
/// a tick or a cross for as long as the reader can see it.
fn copy_glyph(copied: Option<CopiedLine>) -> &'static str {
    match copied {
        Some(CopiedLine { ok: true, .. }) => "✓",
        Some(CopiedLine { ok: false, .. }) => "✕",
        None => "#",
    }
}

/// The glyph's tone right now: the tick and the cross take the clipboard's
/// verdict, and the `#` stays quiet until a copy happens.
fn copy_glyph_class(copied: Option<CopiedLine>) -> &'static str {
    match copied {
        Some(CopiedLine { ok: true, .. }) => "text-success",
        Some(CopiedLine { ok: false, .. }) => "text-destructive",
        None => "text-muted-foreground",
    }
}
