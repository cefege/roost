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
use libero::components::{Box, Button, Pictogram, ScrollArea};
use libero::sx::sx;

use crate::syntax_lite::Token;

use super::state::token_color;

/// The row's box, in pixels. The line-height IS the row height, which is what
/// lets the target marker be placed by arithmetic on the line number.
const ROW_HEIGHT_PX: u32 = 20;

/// The scrollable body: every line, in order, with the target line marked.
#[component]
pub fn FileBody(
    lines: Vec<String>,
    tokens: Option<Vec<Vec<Token>>>,
    target_line: usize,
    copied_line: Option<usize>,
    on_copy: EventHandler<usize>,
) -> Element {
    let scroll_sx = use_hook(|| {
        sx().flex("1")
            .min_height("0")
            .background("var(--surface-terminal)")
            .border_radius("sm")
            .font_family("monospace")
            .font_size("sm")
    });
    let body_sx = use_hook(|| sx().position("relative").padding("sm"));
    let marker_sx = use_hook(|| {
        sx().position("absolute")
            .left("0")
            .width("0.1875rem")
            .background("warning")
    });
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
                copied: copied_line == Some(line_number),
                on_copy,
            }
        });
    }
    let marker_top_px =
        u32::try_from(target_line.saturating_sub(1)).unwrap_or_default() * ROW_HEIGHT_PX;
    rsx! {
        ScrollArea { sx: scroll_sx,
            Box {
                sx: body_sx,
                "data-testid": "file-viewer-sheet-body",
                Box {
                    sx: marker_sx,
                    "data-testid": "file-viewer-sheet-target-marker",
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
    copied: bool,
    on_copy: EventHandler<usize>,
) -> Element {
    let label = format!("Copy link to line {number}");
    let number_sx = use_hook(|| {
        sx().min_width("2.25rem")
            .text_align("right")
            .color("muted")
            .user_select("none")
            .flex_shrink("0")
    });
    let highlighted_sx = use_hook(|| sx().white_space("pre"));
    let plain_sx = use_hook(|| sx().white_space("pre").color("var(--syntax-plain)"));
    let label_sx = use_hook(|| sx().color("muted"));
    let row_sx = use_hook(|| sx().display("flex").gap("md").position("relative"));
    let copy_sx = use_hook(|| {
        sx().margin_left("auto")
            .flex_shrink("0")
            .min_height("0")
            .padding_inline("xs")
            .font_size("xs")
            .opacity("0")
            .selector("[data-copy-row]:hover &", sx().opacity("1"))
            .focus_visible(sx().opacity("1"))
            .media("(hover: none)", sx().opacity("1"))
    });
    let icon_sx = use_hook(|| sx().width("0.75rem").height("0.75rem").color("success"));
    rsx! {
        Box {
            sx: row_sx,
            "data-copy-row": "",
            "data-line": "{number}",
            "data-testid": "file-viewer-sheet-line-{number}",
            style: row_style(is_target),
            Box {
                component: "span",
                sx: number_sx,
                "data-testid": "file-viewer-sheet-line-num-{number}",
                "data-target": is_target.then_some("true"),
                "{number}"
            }
            match tokens {
                Some(tokens) => highlighted_line(&tokens, &highlighted_sx),
                None => plain_line(&text, &plain_sx),
            }
            Button {
                variant: "standard",
                size: "xs",
                sx: copy_sx,
                "data-testid": "file-viewer-sheet-copy-link-{number}",
                title: label.clone(),
                "aria-label": label,
                onclick: move |_| on_copy.call(number),
                if copied {
                    Box { component: "span", sx: icon_sx, Pictogram { icon: pictogram_icons_lucide::check::outlined, width: "0.75rem", height: "0.75rem" } }
                } else {
                    Box { component: "span", sx: label_sx, "#" }
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

fn highlighted_line(tokens: &[Token], highlighted_sx: &libero::sx::Sx) -> Element {
    rsx! {
        Box {
            component: "span",
            sx: highlighted_sx.clone(),
            for token in tokens {
                span { style: "color: {token_color(token.kind)};", {token.text.clone()} }
            }
        }
    }
}

/// The line's text, unhighlighted.
fn plain_line(line: &str, plain_sx: &libero::sx::Sx) -> Element {
    rsx! {
        Box { component: "span", sx: plain_sx.clone(), {line} }
    }
}
