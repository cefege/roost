//! Native `diff` blocks: structured hunks or unified text, with old/new line
//! gutters, gap rows between regions, tone colours and full-source copy.

use super::super::prop_text;
use super::copy::CopyButton;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;

pub(super) fn render(node: &OmpUiNode) -> Element {
    let text = prop_text(node, "text");
    let hunks = node.p.get("hunks").and_then(Value::as_array);
    let (rows, copy) = if let Some(hunks) = hunks.filter(|hunks| !hunks.is_empty()) {
        hunk_rows(hunks)
    } else {
        unified_rows(&text)
    };
    let path = prop_text(node, "path");
    let lang = prop_text(node, "lang");
    let label = if !path.is_empty() { path } else { lang };
    rsx! {
        div { class: "flex min-w-0 flex-col rounded-none border border-border",
            div { class: "flex min-w-0 items-center justify-between gap-2 border-b border-border px-2 py-1",
                span { class: "min-w-0 truncate font-mono text-xs text-muted-foreground",
                    if label.is_empty() { "diff" } else { "{label}" }
                }
                CopyButton { text: copy }
            }
            pre { class: "m-0 overflow-x-auto bg-terminal p-2 font-mono text-xs leading-5 text-terminal-foreground",
                    for (index, row) in rows.iter().enumerate() {
                        div {
                            class: row.classes(),
                            "data-line": row.new.or(row.old).unwrap_or((index + 1) as u64),
                            "data-kind": row.kind(),
                            span { class: "w-10 flex-none select-none text-right text-muted-foreground",
                                if let Some(number) = row.old { "{number}" } else { "" }
                            }
                            span { class: "w-10 flex-none select-none text-right text-muted-foreground",
                                if let Some(number) = row.new { "{number}" } else { "" }
                            }
                            span { class: "w-4 flex-none select-none text-center", "{row.sign}" }
                            span { class: "min-w-0 whitespace-pre", "{row.content}" }
                        }
                    }
                }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Added,
    Removed,
    Context,
    Meta,
    Gap,
}

struct DiffRow {
    old: Option<u64>,
    new: Option<u64>,
    sign: &'static str,
    content: String,
    tone: Tone,
}

impl DiffRow {
    fn classes(&self) -> &'static str {
        match self.tone {
            Tone::Added => "flex min-w-0 gap-2 bg-success/10 text-success-dark",
            Tone::Removed => "flex min-w-0 gap-2 bg-destructive/10 text-destructive",
            Tone::Context => "flex min-w-0 gap-2",
            Tone::Meta => "flex min-w-0 gap-2 text-muted-foreground",
            Tone::Gap => "flex min-w-0 gap-2 text-muted-foreground",
        }
    }

    fn kind(&self) -> &'static str {
        match self.tone {
            Tone::Added => "added",
            Tone::Removed => "removed",
            Tone::Context => "context",
            Tone::Meta => "meta",
            Tone::Gap => "gap",
        }
    }
}

fn hunk_rows(hunks: &[Value]) -> (Vec<DiffRow>, String) {
    let mut rows = Vec::new();
    let mut copy = String::new();
    let mut previous_old: Option<u64> = None;
    for hunk in hunks {
        let old_start = hunk.get("oldStart").and_then(Value::as_u64).unwrap_or(1);
        let new_start = hunk.get("newStart").and_then(Value::as_u64).unwrap_or(1);
        if let Some(previous) = previous_old
            && old_start > previous.saturating_add(1)
        {
            rows.push(gap_row());
            copy.push_str("…\n");
        }
        if !copy.is_empty() {
            copy.push('\n');
        }
        copy.push_str(&format!("@@ -{old_start} +{new_start} @@"));
        rows.push(DiffRow {
            old: None,
            new: None,
            sign: "",
            content: format!("@@ -{old_start} +{new_start} @@"),
            tone: Tone::Meta,
        });
        let mut old = old_start;
        let mut new = new_start;
        for line in hunk
            .get("lines")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let (prefix, content) = match line.chars().next() {
                Some(character) => (&line[..character.len_utf8()], &line[character.len_utf8()..]),
                None => ("", ""),
            };
            let (row_old, row_new, tone) = match prefix {
                "+" => {
                    let number = new;
                    new = new.saturating_add(1);
                    (None, Some(number), Tone::Added)
                }
                "-" => {
                    let number = old;
                    old = old.saturating_add(1);
                    (Some(number), None, Tone::Removed)
                }
                " " => {
                    let before = old;
                    let after = new;
                    old = old.saturating_add(1);
                    new = new.saturating_add(1);
                    (Some(before), Some(after), Tone::Context)
                }
                _ => (None, None, Tone::Meta),
            };
            rows.push(DiffRow {
                old: row_old,
                new: row_new,
                sign: match tone {
                    Tone::Added => "+",
                    Tone::Removed => "-",
                    _ => " ",
                },
                content: content.to_owned(),
                tone,
            });
            copy.push('\n');
            copy.push_str(line);
        }
        previous_old = Some(old.saturating_sub(1));
    }
    (rows, copy)
}

fn unified_rows(text: &str) -> (Vec<DiffRow>, String) {
    let mut rows = Vec::new();
    let mut old = None;
    let mut new = None;
    let mut previous_old: Option<u64> = None;
    for line in text.split('\n') {
        if let Some((old_start, new_start)) = parse_hunk_header(line) {
            if let Some(previous) = previous_old
                && old_start > previous.saturating_add(1)
            {
                rows.push(gap_row());
            }
            old = Some(old_start);
            new = Some(new_start);
            rows.push(DiffRow {
                old: None,
                new: None,
                sign: "",
                content: line.to_owned(),
                tone: Tone::Meta,
            });
            continue;
        }
        let (tone, sign, content) = match line.chars().next() {
            Some('+') if !line.starts_with("+++") => (Tone::Added, "+", &line[1..]),
            Some('-') if !line.starts_with("---") => (Tone::Removed, "-", &line[1..]),
            Some(' ') => (Tone::Context, " ", &line[1..]),
            _ => (Tone::Meta, "", line),
        };
        let (row_old, row_new) = match tone {
            Tone::Added => {
                let number = new;
                new = new.map(|number| number.saturating_add(1));
                (None, number)
            }
            Tone::Removed => {
                let number = old;
                old = old.map(|number| number.saturating_add(1));
                (number, None)
            }
            Tone::Context => {
                let numbers = (old, new);
                old = old.map(|number| number.saturating_add(1));
                new = new.map(|number| number.saturating_add(1));
                numbers
            }
            _ => (None, None),
        };
        if row_old.is_some() {
            previous_old = row_old;
        }
        rows.push(DiffRow {
            old: row_old,
            new: row_new,
            sign,
            content: content.to_owned(),
            tone,
        });
    }
    (rows, text.to_owned())
}

fn parse_hunk_header(line: &str) -> Option<(u64, u64)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old_start, rest) = rest.split_once(' ')?;
    let new_start = rest.strip_prefix('+')?;
    let old_start = old_start.split(',').next()?.parse().ok()?;
    let new_start = new_start
        .split(',')
        .next()?
        .split(' ')
        .next()?
        .parse()
        .ok()?;
    Some((old_start, new_start))
}

fn gap_row() -> DiffRow {
    DiffRow {
        old: None,
        new: None,
        sign: "…",
        content: String::new(),
        tone: Tone::Gap,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{Tone, parse_hunk_header, unified_rows};

    #[test]
    fn unified_hunk_numbers_follow_the_header() {
        let (rows, copy) = unified_rows("@@ -3,2 +4,2 @@\n same\n-old\n+new\n");
        assert!(copy.starts_with("@@ -3,2"));
        let old_row = rows.iter().find(|row| row.tone == Tone::Removed).unwrap();
        assert_eq!(old_row.old, Some(4));
        assert_eq!(old_row.new, None);
        let new_row = rows.iter().find(|row| row.tone == Tone::Added).unwrap();
        assert_eq!(new_row.old, None);
        assert_eq!(new_row.new, Some(5));
    }

    #[test]
    fn malformed_headers_do_not_panic() {
        assert_eq!(parse_hunk_header("@@ -x +y @@"), None);
        assert_eq!(parse_hunk_header("not a header"), None);
    }
}
