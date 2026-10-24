//! The edit box's diff body: OMP's numbered unified diff with `+`/`-` tone,
//! word-level highlights on single-line replacements, visible whitespace, and
//! the hunk/line fold. The row model is `diff_model`; this file only lays it out.

use super::diff_model::{
    COLLAPSED_HUNKS, COLLAPSED_LINES, DiffRow, RowKind, WordRun, fold, gutter_width, parse_unified,
};
use dioxus::prelude::*;
use libero::components::{Box, Button, Text};
use libero::sx::sx;

#[component]
pub fn DiffBody(diff: String, open: bool, on_expand: EventHandler<bool>) -> Element {
    let rows = parse_unified(&diff);
    let gutter = gutter_width(&rows);
    let capped = fold(&rows, COLLAPSED_HUNKS, COLLAPSED_LINES);
    let overflows = capped.hidden_hunks + capped.hidden_lines > 0;
    let hint = format!(
        "… ({} more hunks, {} more lines)",
        capped.hidden_hunks, capped.hidden_lines
    );
    let folded = if open {
        fold(&rows, usize::MAX, usize::MAX)
    } else {
        capped
    };
    let diff_sx = use_hook(|| {
        sx().overflow_x("auto")
            .with("overscroll-behavior-y", "auto")
            .border_radius("sm")
            .border_width("1px")
            .border_color("var(--border)")
            .font_family("monospace")
            .font_size("xs")
            .line_height("1.25rem")
    });
    let hint_sx = use_hook(|| {
        sx().margin("0")
            .padding_inline("md")
            .padding_block("xs")
            .font_size("xs")
            .color("muted")
    });
    rsx! {
        Box { component: "div", sx: diff_sx, style: "--diff-gutter: {gutter};",
            for (idx, row) in folded.rows.iter().enumerate() {
                DiffLine { key: "{idx}", row: row.clone() }
            }
        }
        if overflows && !open {
            Text { component: "p", sx: hint_sx, "{hint}" }
            Button { variant: "standard", color: "primary", size: "sm",
                onclick: move |_| on_expand.call(true), "Show all" }
        }
        if overflows && open {
            Button { variant: "standard", color: "primary", size: "sm",
                onclick: move |_| on_expand.call(false), "Collapse" }
        }
    }
}

#[component]
fn DiffLine(row: DiffRow) -> Element {
    let kind = kind_name(row.kind);
    let row_sx = use_hook(|| {
        sx().display("flex")
            .min_width("100%")
            .selector(
                "&[data-kind='file'], &[data-kind='hunk']",
                sx().background("muted").color("muted"),
            )
            .selector(
                "&[data-kind='added']",
                sx().background_color("color-mix(in srgb, var(--primary) 10%, transparent)"),
            )
            .selector(
                "&[data-kind='removed']",
                sx().background_color("color-mix(in srgb, var(--destructive) 10%, transparent)"),
            )
            .selector(
                "&[data-kind='added'] [data-part='sign']",
                sx().color("primary"),
            )
            .selector(
                "&[data-kind='removed'] [data-part='sign']",
                sx().color("error"),
            )
    });
    let text_sx = use_hook(|| {
        sx().min_width("0")
            .flex("1")
            .white_space("pre-wrap")
            .with("overflow-wrap", "anywhere")
            .padding_right("md")
    });
    let heading_text_sx = use_hook(|| {
        sx().min_width("0")
            .flex("1")
            .white_space("pre-wrap")
            .with("overflow-wrap", "anywhere")
            .padding_inline("md")
    });
    let line_number_sx = use_hook(|| {
        sx().width("calc(var(--diff-gutter) * 1ch)")
            .flex_shrink("0")
            .with("user-select", "none")
            .padding_right("sm")
            .text_align("right")
            .color("muted")
    });
    let sign_sx = use_hook(|| {
        sx().width("1rem")
            .flex_shrink("0")
            .with("user-select", "none")
            .text_align("center")
    });
    let run_sx = use_hook(|| {
        sx().selector(
            "&[data-changed=true]",
            sx().background_color("color-mix(in srgb, var(--primary) 25%, transparent)"),
        )
    });
    let whitespace_sx = use_hook(|| sx().color("muted").opacity("0.6"));
    if matches!(row.kind, RowKind::File | RowKind::Hunk) {
        return rsx! {
            Box { component: "div", sx: row_sx, "data-kind": kind,
                Text { component: "span", sx: heading_text_sx, "{row.text}" }
            }
        };
    }
    let line_no = row
        .line_no
        .map(|number| number.to_string())
        .unwrap_or_default();
    let sign = sign_mark(row.kind);
    let runs = if row.runs.is_empty() {
        vec![WordRun {
            text: row.text.clone(),
            changed: false,
        }]
    } else {
        row.runs.clone()
    };
    rsx! {
        Box { component: "div", sx: row_sx, "data-kind": kind,
            Text { component: "span", sx: line_number_sx, "{line_no}" }
            Text { component: "span", sx: sign_sx, "data-part": "sign", "{sign}" }
            Box { component: "span", sx: text_sx,
                for run in runs.iter() {
                    Box { component: "span", sx: run_sx.clone(), "data-changed": run.changed.then_some("true"),
                        for piece in visible_pieces(&run.text) {
                            {match piece {
                                Piece::Text(text) => rsx! { span { "{text}" } },
                                Piece::Tab => rsx! { Text { component: "span", sx: whitespace_sx.clone(), "→" } },
                                Piece::Space => rsx! { Text { component: "span", sx: whitespace_sx.clone(), "·" } },
                            }}
                        }
                    }
                }
            }
        }
    }
}

/// One stretch of a diff line: text, or a whitespace character drawn as a
/// visible marker (OMP shows tabs as `→` and spaces as `·`).
#[derive(Debug, PartialEq, Eq)]
enum Piece {
    Text(String),
    Tab,
    Space,
}

fn visible_pieces(text: &str) -> Vec<Piece> {
    let mut pieces: Vec<Piece> = Vec::new();
    for ch in text.chars() {
        match ch {
            '\t' => pieces.push(Piece::Tab),
            ' ' => pieces.push(Piece::Space),
            other => match pieces.last_mut() {
                Some(Piece::Text(buffer)) => buffer.push(other),
                _ => pieces.push(Piece::Text(other.to_string())),
            },
        }
    }
    pieces
}

fn kind_name(kind: RowKind) -> &'static str {
    match kind {
        RowKind::File => "file",
        RowKind::Hunk => "hunk",
        RowKind::Context => "context",
        RowKind::Added => "added",
        RowKind::Removed => "removed",
    }
}

fn sign_mark(kind: RowKind) -> &'static str {
    match kind {
        RowKind::Added => "+",
        RowKind::Removed => "-",
        _ => " ",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{Piece, visible_pieces};

    #[test]
    fn tabs_and_spaces_become_markers_around_the_text() {
        assert_eq!(
            visible_pieces("a\tb c"),
            vec![
                Piece::Text("a".to_owned()),
                Piece::Tab,
                Piece::Text("b".to_owned()),
                Piece::Space,
                Piece::Text("c".to_owned()),
            ]
        );
    }
}
