//! Detects inferred terminal links across rendered rows, including soft wraps.
//! `links::anchor` (the DOM linkifier's plan) consumes these pure row segments;
//! target validation stays in `link_target` for painted and inferred links
//! alike. Offsets are UTF-16 code units — what a DOM `Range` addresses — and the
//! patterns live in `detect/patterns.rs` and `detect/file_path.rs`.
//! Ports `apps/web/src/renderer/terminal-links.detect.ts`.

mod file_path;
mod match_list;
mod patterns;

use crate::link_target::ResolveFile;
use match_list::find_matches;
use patterns::{is_link_char, is_lower_hex};

/// One PAINTED producer link: an OSC 8 anchor the row painter already placed at
/// the cells the core authored. Offsets are code units, row-local on input.
/// `key` is the core's run identity; two halves of one wrapped link share it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaintedLink {
    /// First code unit of the anchor's text.
    pub start: usize,
    /// Code unit just past the anchor's text.
    pub end: usize,
    /// The producer's exact target.
    pub uri: String,
    /// The core's run key (`LINK_KEY_ATTR`).
    pub key: String,
}

/// One visual row's input to detection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowLinkInput {
    /// The row's rendered text.
    pub text: String,
    /// The row's GRID OCCUPANCY in columns (`ROW_COLUMNS_ATTR`), never a text
    /// length: the soft-wrap test is "did this row fill the grid?", and a wide
    /// glyph or an emoji cluster makes code units disagree with columns.
    pub columns: i64,
    /// Producer links already painted on this row, ascending by `start`.
    pub links: Vec<PaintedLink>,
}

/// The file-only half of a segment. `source` is the exact terminal-authored
/// target, kept apart from the route so activation reclassifies rather than
/// trusting mutable anchor attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLinkSegment {
    /// The hover hint, `Open <path>`.
    pub hint: String,
    /// The terminal-authored path text.
    pub source: String,
}

/// One link segment to wrap in a single visual row (row-local offsets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowLinkSegment {
    /// Index of the row in the input.
    pub row: usize,
    /// First code unit to wrap.
    pub start: usize,
    /// Code unit just past the wrap.
    pub end: usize,
    /// The href: an absolute HTTP(S) URL or an authenticated worker route.
    pub url: String,
    /// Present only for a file link.
    pub file: Option<FileLinkSegment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchKind {
    Url,
    File,
    Blocked,
}

#[derive(Debug)]
struct Match {
    start: usize,
    end: usize,
    url: String,
    kind: MatchKind,
    hint: Option<String>,
    source: Option<String>,
    /// Already an anchor in the DOM: seeded for precedence, never returned.
    painted: bool,
}

/// v2 `ROW_LINK_HINT`: `/[:/.#\\]|[0-9a-f]{7}/`. A logical line without one
/// cannot match any pattern, so the regex battery is skipped for it.
fn has_link_hint(text: &[u16]) -> bool {
    let mut hex_run = 0;
    text.iter().any(|unit| {
        if matches!(u8::try_from(*unit), Ok(b':' | b'/' | b'.' | b'#' | b'\\')) {
            return true;
        }
        hex_run = if is_lower_hex(*unit) { hex_run + 1 } else { 0 };
        hex_run >= 7
    })
}

/// Group soft-wrapped rows into logical lines, detect links on each JOINED
/// line, and return the per-row segments still to wrap. A row whose column
/// occupancy reaches `cols` wrapped into the next; `cols <= 0` never joins.
/// Non-URL border decoration flanking a wrap boundary is stripped before
/// joining, and a row with no URL character stays verbatim as a separator.
pub fn compute_row_links(
    rows: &[RowLinkInput],
    cols: i64,
    resolve_file: Option<ResolveFile<'_>>,
    github_owner_repo: Option<&str>,
) -> Vec<RowLinkSegment> {
    let texts: Vec<Vec<u16>> = rows
        .iter()
        .map(|row| row.text.encode_utf16().collect())
        .collect();
    let mut out = Vec::new();
    let mut first = 0;
    while first < rows.len() {
        let mut last = first;
        while cols > 0 && rows[last].columns >= cols && last + 1 < rows.len() {
            last += 1;
        }
        let mut joined: Vec<u16> = Vec::new();
        let mut bases = Vec::new();
        let mut lead_skips = Vec::new();
        let mut row_lens = Vec::new();
        for (index, text) in texts.iter().enumerate().take(last + 1).skip(first) {
            let mut lead = 0;
            if index > first {
                lead = text
                    .iter()
                    .position(|unit| is_link_char(*unit))
                    .unwrap_or(text.len());
            }
            let mut trail = text.len();
            if index < last {
                while trail > lead && !is_link_char(text[trail - 1]) {
                    trail -= 1;
                }
            }
            let cleaned = &text[lead..trail];
            // A row reduced to no URL character is decoration between two URLs:
            // kept verbatim, its borders break the match instead of fusing them.
            let used_lead = if cleaned.iter().any(|unit| is_link_char(*unit)) {
                lead
            } else {
                0
            };
            let used = if used_lead == lead {
                cleaned
            } else {
                text.as_slice()
            };
            lead_skips.push(used_lead);
            row_lens.push(used.len());
            bases.push(joined.len());
            joined.extend_from_slice(used);
        }
        let painted = joined_painted_links(&rows[first..=last], &bases, &lead_skips, &row_lens);
        if !joined.is_empty() && has_link_hint(&joined) {
            for found in find_matches(&joined, painted, resolve_file, github_owner_repo) {
                for group_row in 0..=(last - first) {
                    let base = bases[group_row];
                    let start = found.start.max(base);
                    let end = found.end.min(base + row_lens[group_row]);
                    if start >= end {
                        continue;
                    }
                    let offset = lead_skips[group_row];
                    let file = (found.kind == MatchKind::File).then(|| FileLinkSegment {
                        hint: found.hint.clone().unwrap_or_default(),
                        source: found.source.clone().unwrap_or_default(),
                    });
                    out.push(RowLinkSegment {
                        row: first + group_row,
                        start: start - base + offset,
                        end: end - base + offset,
                        url: found.url.clone(),
                        file,
                    });
                }
            }
        }
        first = last + 1;
    }
    out
}

/// Painted links lifted from row-local into joined-line offsets. Two halves of
/// one soft-wrapped link are adjacent here and share the core's run key, so
/// they fuse into ONE match that an overlapping inference decides against.
fn joined_painted_links(
    group: &[RowLinkInput],
    bases: &[usize],
    lead_skips: &[usize],
    row_lens: &[usize],
) -> Vec<PaintedLink> {
    let mut painted: Vec<PaintedLink> = Vec::new();
    for (index, row) in group.iter().enumerate() {
        let (base, offset, length) = (bases[index], lead_skips[index], row_lens[index]);
        for link in &row.links {
            let start = link.start.saturating_sub(offset);
            let end = length.min(link.end.saturating_sub(offset));
            if start >= end {
                continue;
            }
            match painted.last_mut() {
                Some(last) if last.key == link.key && last.end == base + start => {
                    last.end = base + end;
                }
                _ => painted.push(PaintedLink {
                    start: base + start,
                    end: base + end,
                    uri: link.uri.clone(),
                    key: link.key.clone(),
                }),
            }
        }
    }
    painted
}
