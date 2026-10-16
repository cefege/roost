//! The edit box's diff body: OMP's numbered unified diff with `+`/`-` tone,
//! word-level highlights on single-line replacements, visible whitespace, and
//! the hunk/line fold. The row model is `diff_model`; this file only lays it out.

use dioxus::prelude::*;

use super::diff_model::{
    COLLAPSED_HUNKS, COLLAPSED_LINES, DiffRow, RowKind, WordRun, fold, gutter_width, parse_unified,
};
use crate::components::md::{Button, ButtonSize, ButtonVariant};

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
    rsx! {
        div { class: "agent-chat__diff", style: "--diff-gutter: {gutter};",
            for (idx, row) in folded.rows.iter().enumerate() {
                DiffLine { key: "{idx}", row: row.clone() }
            }
        }
        if overflows && !open {
            p { class: "agent-chat__step-hint", "{hint}" }
            Button { variant: ButtonVariant::Link, size: ButtonSize::Sm, class: "agent-chat__step-more",
                onclick: move |_| on_expand.call(true), "Show all" }
        }
        if overflows && open {
            Button { variant: ButtonVariant::Link, size: ButtonSize::Sm, class: "agent-chat__step-more",
                onclick: move |_| on_expand.call(false), "Collapse" }
        }
    }
}

#[component]
fn DiffLine(row: DiffRow) -> Element {
    let kind = kind_name(row.kind);
    if matches!(row.kind, RowKind::File | RowKind::Hunk) {
        return rsx! {
            div { class: "agent-chat__diff-row agent-chat__diff-meta", "data-kind": kind,
                span { class: "agent-chat__diff-text", "{row.text}" }
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
        div { class: "agent-chat__diff-row", "data-kind": kind,
            span { class: "agent-chat__diff-gutter", "{line_no}" }
            span { class: "agent-chat__diff-sign", "{sign}" }
            span { class: "agent-chat__diff-text",
                for run in runs.iter() {
                    span { "data-changed": run.changed.then_some("true"),
                        for piece in visible_pieces(&run.text) {
                            {match piece {
                                Piece::Text(text) => rsx! { span { "{text}" } },
                                Piece::Tab => rsx! { span { class: "agent-chat__diff-tab", "→" } },
                                Piece::Space => rsx! { span { class: "agent-chat__diff-space", "·" } },
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
