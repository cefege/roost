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

use crate::components::md::{Button, ButtonSize, ButtonVariant};
use crate::syntax_lite::Token;

use super::state::token_color;

/// The row's box, in pixels. The line-height IS the row height, which is what
/// lets the target marker be placed by arithmetic on the line number.
const ROW_HEIGHT_PX: u32 = 20;

/// The gutter the line number sits in, so the text starts in the same column on
/// every row whatever the file's line count is.
const GUTTER_MIN_WIDTH: &str = "36px";

/// How long a copied line shows its tick or cross before the row goes back to
/// `#`. v2's copy feedback window.
pub const COPY_FEEDBACK_MS: u32 = 1500;

/// The one rule the control needs and an inline style cannot: a row's control
/// is invisible until the row is under the pointer.
const COPY_HOVER_RULE: &str = ".fvs-line:hover .fvs-copy-btn { opacity: 1 !important; }";

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
        style { {COPY_HOVER_RULE} }
        div {
            "data-testid": "file-viewer-sheet-body",
            style: "overflow-y: auto; font-family: var(--font-mono); font-size: var(--md-body-s-size); flex: 1 1 auto; min-height: 0; background: var(--bg-base); border-radius: var(--md-shape-xs); padding: var(--md-space-2); position: relative;",
            div {
                "data-testid": "file-viewer-sheet-target-marker",
                style: "position: absolute; left: 0; width: 3px; top: {marker_top_px}px; height: {ROW_HEIGHT_PX}px; background: var(--color-warn);"
            }
            {rows.into_iter()}
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
            class: "fvs-line",
            style: row_style(is_target),
            span {
                "data-testid": "file-viewer-sheet-line-num-{number}",
                "data-target": is_target.then_some("true"),
                style: "color: var(--text-lo); min-width: {GUTTER_MIN_WIDTH}; text-align: right; user-select: none; flex-shrink: 0;",
                "{number}"
            }
            match tokens {
                Some(tokens) => highlighted_line(&tokens),
                None => plain_line(&text),
            }
            Button {
                variant: ButtonVariant::Ghost,
                size: ButtonSize::Xs,
                class: "fvs-copy-btn",
                "data-testid": "file-viewer-sheet-copy-link-{number}",
                title: label.clone(),
                "aria-label": label,
                style: copy_button_style(copied),
                onclick: move |_| on_copy.call(number),
                {copy_glyph(copied)}
            }
        }
    }
}

/// One line's row: the target line is tinted so the eye lands on it, and the
/// control only appears on hover so a body of two thousand lines is not two
/// thousand controls.
fn row_style(is_target: bool) -> String {
    let highlight = if is_target {
        "background: color-mix(in srgb, var(--ansi-bright-yellow) 10%, transparent);"
    } else {
        ""
    };
    format!(
        "display: flex; gap: var(--md-space-3); line-height: {ROW_HEIGHT_PX}px; position: relative; {highlight}"
    )
}

/// The line's text, one span per token so each run is painted in its own colour.
fn highlighted_line(tokens: &[Token]) -> Element {
    rsx! {
        span {
            style: "white-space: pre;",
            for token in tokens {
                span { style: "color: {token_color(token.kind)};", {token.text.clone()} }
            }
        }
    }
}

/// The line's text, unhighlighted.
fn plain_line(line: &str) -> Element {
    rsx! {
        span { style: "color: var(--syntax-plain); white-space: pre;", {line} }
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

/// The control's own styling, including the colour its glyph is in right now.
fn copy_button_style(copied: Option<CopiedLine>) -> String {
    let color = match copied {
        Some(CopiedLine { ok: true, .. }) => "var(--color-ok)",
        Some(CopiedLine { ok: false, .. }) => "var(--color-err)",
        None => "var(--text-lo)",
    };
    format!(
        "margin-left: auto; flex-shrink: 0; min-height: 0; padding: 0 var(--md-space-1); font-size: var(--md-label-s-size); background: none; border: none; opacity: 0; color: {color};"
    )
}
