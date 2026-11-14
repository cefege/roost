//! Side-by-side native diffs pair each replacement run without copying text.
//! The semantic diff renderer calls this after parsing hunks or unified text.
//! Shared metadata spans both columns; context keeps both line numbers.

use super::{DiffRow, Tone};
use dioxus::prelude::*;

pub(super) fn render(rows: &[DiffRow], wrap: bool) -> Element {
    let (width, text_layout) = if wrap {
        ("w-full", "min-w-0 whitespace-pre-wrap wrap-anywhere")
    } else {
        ("w-max", "whitespace-pre")
    };
    rsx! {
        div { class: "min-w-0 max-w-full overflow-x-auto bg-terminal p-2 text-terminal-foreground",
        div { class: "grid {width} min-w-full grid-cols-2 font-mono text-xs leading-5",
            for line in SplitRows::new(rows) {
                match line {
                    SplitLine::Full(row) => rsx! {
                        div { class: "col-span-2",
                            div { class: row.classes(),
                                span { class: "w-4 flex-none select-none text-center", "{row.sign}" }
                                span { class: "{text_layout}", "{row.content}" }
                            }
                        }
                    },
                    SplitLine::Pair(old, new) => rsx! {
                        {cell(old, false, text_layout)}
                        {cell(new, true, text_layout)}
                    },
                }
            }
        }
        }
    }
}

fn cell(row: Option<&DiffRow>, new: bool, text_layout: &'static str) -> Element {
    let Some(row) = row else {
        return rsx! { div { class: "min-w-0 border-r border-border bg-muted/20" } };
    };
    let number = if new { row.new } else { row.old };
    rsx! {
        div { class: "min-w-0 border-r border-border bg-terminal text-terminal-foreground",
            div { class: row.classes(), "data-kind": row.kind(),
                span { class: "w-10 flex-none select-none text-right text-muted-foreground",
                    if let Some(number) = number { "{number}" }
                }
                span { class: "w-4 flex-none select-none text-center", "{row.sign}" }
                span { class: "{text_layout}", "{row.content}" }
            }
        }
    }
}

enum SplitLine<'a> {
    Full(&'a DiffRow),
    Pair(Option<&'a DiffRow>, Option<&'a DiffRow>),
}

struct SplitRows<'a> {
    rows: &'a [DiffRow],
    cursor: usize,
    removed: std::slice::Iter<'a, DiffRow>,
    added: std::slice::Iter<'a, DiffRow>,
}

impl<'a> SplitRows<'a> {
    fn new(rows: &'a [DiffRow]) -> Self {
        Self {
            rows,
            cursor: 0,
            removed: [].iter(),
            added: [].iter(),
        }
    }
}

impl<'a> Iterator for SplitRows<'a> {
    type Item = SplitLine<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let old = self.removed.next();
        let new = self.added.next();
        if old.is_some() || new.is_some() {
            return Some(SplitLine::Pair(old, new));
        }
        let row = self.rows.get(self.cursor)?;
        if matches!(row.tone, Tone::Removed | Tone::Added) {
            let start = self.cursor;
            while self
                .rows
                .get(self.cursor)
                .is_some_and(|row| row.tone == Tone::Removed)
            {
                self.cursor += 1;
            }
            let middle = self.cursor;
            while self
                .rows
                .get(self.cursor)
                .is_some_and(|row| row.tone == Tone::Added)
            {
                self.cursor += 1;
            }
            self.removed = self.rows[start..middle].iter();
            self.added = self.rows[middle..self.cursor].iter();
            return Some(SplitLine::Pair(self.removed.next(), self.added.next()));
        }
        self.cursor += 1;
        Some(if row.tone == Tone::Context {
            SplitLine::Pair(Some(row), Some(row))
        } else {
            SplitLine::Full(row)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::unified_rows;
    use super::*;

    #[test]
    fn unequal_replacements_do_not_pair_across_context_or_hunks() {
        let (rows, _) = unified_rows(
            "@@ -1,4 +1,3 @@\n-old one\n-old two\n+new\n same\n-old three\n@@ -20 +30 @@\n+inserted",
        );
        let actual: Vec<_> = SplitRows::new(&rows)
            .map(|line| match line {
                SplitLine::Full(_) => (None, None),
                SplitLine::Pair(old, new) => (
                    old.map(|row| (row.old, row.content.as_str())),
                    new.map(|row| (row.new, row.content.as_str())),
                ),
            })
            .collect();
        assert_eq!(
            actual,
            vec![
                (None, None),
                (Some((Some(1), "old one")), Some((Some(1), "new"))),
                (Some((Some(2), "old two")), None),
                (Some((Some(3), "same")), Some((Some(2), "same"))),
                (Some((Some(4), "old three")), None),
                (None, None),
                (None, None),
                (None, Some((Some(30), "inserted"))),
            ]
        );
    }
}
