//! Column-preserving text-run grouping for optional terminal ligatures.
//!
//! The DOM painter consumes these ranges only for contiguous ordinary cells.
//! Atomic glyphs and visual boundaries stay separate so every overlay remains
//! addressed in the worker's grid columns.

use roost_protocol::cell::{CellSpan, span_is_atomic};

/// A maximal half-open range of spans that can shape as one text run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LigatureRun {
    /// First span index.
    pub start: usize,
    /// One past the last span index.
    pub end: usize,
}

/// Partition spans at style, link, and atomic-glyph boundaries.
///
/// A caller with find hits must also split at the hit boundaries; this function
/// deliberately returns singleton runs for the whole row in that case.
pub fn ligature_runs(spans: &[CellSpan], has_find_hits: bool) -> Vec<LigatureRun> {
    if has_find_hits {
        return (0..spans.len())
            .map(|index| LigatureRun {
                start: index,
                end: index + 1,
            })
            .collect();
    }
    let mut runs = Vec::new();
    let mut start = 0;
    while start < spans.len() {
        let first = &spans[start];
        if span_is_atomic(first) {
            runs.push(LigatureRun {
                start,
                end: start + 1,
            });
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < spans.len() && can_join(first, &spans[end - 1], &spans[end]) {
            end += 1;
        }
        runs.push(LigatureRun { start, end });
        start = end;
    }
    runs
}

fn can_join(first: &CellSpan, previous: &CellSpan, next: &CellSpan) -> bool {
    !span_is_atomic(next)
        && same_style(first, next)
        && previous.link_uri == next.link_uri
        && previous.link_key == next.link_key
}

fn same_style(left: &CellSpan, right: &CellSpan) -> bool {
    left.fg == right.fg
        && left.bg == right.bg
        && left.flags == right.flags
        && left.fg_rgb == right.fg_rgb
        && left.bg_rgb == right.bg_rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, columns: u32) -> CellSpan {
        CellSpan {
            text: text.to_owned(),
            fg: 7,
            bg: 256,
            flags: 0,
            fg_rgb: None,
            bg_rgb: None,
            columns,
            link_uri: None,
            link_key: None,
        }
    }

    fn ranges(spans: &[CellSpan], has_find_hits: bool) -> Vec<(usize, usize)> {
        ligature_runs(spans, has_find_hits)
            .into_iter()
            .map(|run| (run.start, run.end))
            .collect()
    }

    #[test]
    fn joins_matching_narrow_spans_but_splits_every_painted_boundary() {
        let spans = vec![
            span("f", 1),
            span("i", 1),
            CellSpan {
                fg: 2,
                ..span("x", 1)
            },
            span("界", 2),
            span("z", 1),
        ];
        assert_eq!(ranges(&spans, false), vec![(0, 2), (2, 3), (3, 4), (4, 5)]);
    }

    #[test]
    fn find_matches_force_boundaries_before_highlight_subspans_are_painted() {
        let spans = vec![span("f", 1), span("i", 1)];
        assert_eq!(ranges(&spans, true), vec![(0, 1), (1, 2)]);
    }

    #[test]
    fn links_with_different_run_keys_never_merge() {
        let left = CellSpan {
            link_uri: Some("https://example.test/".to_owned()),
            link_key: Some("first".to_owned()),
            ..span("f", 1)
        };
        let right = CellSpan {
            link_uri: Some("https://example.test/".to_owned()),
            link_key: Some("second".to_owned()),
            ..span("i", 1)
        };
        assert_eq!(ranges(&[left, right], false), vec![(0, 1), (1, 2)]);
    }
}
