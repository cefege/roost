//! What a scrollback search is matching, and how one row's text is turned into
//! hits. `browser_commands::search_scan` drives it over
//! `crate::session::retained_grid`'s rows; nothing here knows about a request
//! or a result.
//!
//! It is v2's `apps/worker/src/terminal/search/terminal-search-matcher.ts` plus
//! the two conversions the scan needs around it, and it exists as its own file
//! because the matcher is the one part of a search with a shape a reader wants
//! to check on its own: what compiles, what does not, and what a match offset
//! means.

use roost_protocol::cell::{spans_text, text_range_to_columns};
use roost_protocol::terminal_search::TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS;

use super::Refusal;
use crate::session::types::SessionRecord;

/// The command a refusal is attributed to. A scan answers its own result with a
/// stop reason, so the only refusals here are the ones that never reach a scan.
const COMMAND: &str = "search-scrollback";

/// The query as the frame carried it, before it is compiled.
#[derive(Debug, Clone, Copy)]
pub(super) enum Scan<'a> {
    Literal {
        query: &'a str,
        regex: bool,
        case_sensitive: bool,
    },
}

impl Scan<'_> {
    pub(super) fn query(&self) -> &str {
        match self {
            Self::Literal { query, .. } => query,
        }
    }
}

/// A compiled query, or the literal scan that needs no engine.
#[derive(Debug)]
pub(super) enum Matcher {
    /// A case-sensitive substring.
    ///
    /// v2 skips the engine for exactly this combination, and it is worth
    /// keeping: a literal scan is linear in the row and cannot time out on a
    /// pattern, which is the whole point of the deadline the scan carries.
    Literal(String),
    Pattern(regex::Regex),
}

impl Matcher {
    /// Compile the query, or refuse it with a cause the caller can show.
    ///
    /// A pattern that does not compile is REFUSED rather than treated as a
    /// literal. Silently searching for the source text of a broken pattern is
    /// how a user ends up told "no matches" when they meant "your pattern is
    /// wrong" — and the cap exists so a pathological pattern cannot spend the
    /// whole work budget before the first row is read.
    pub(super) fn compile(request: Scan<'_>) -> Result<Self, Refusal> {
        let (query, is_regex, case_sensitive) = match request {
            Scan::Literal {
                query,
                regex,
                case_sensitive,
            } => (query, regex, case_sensitive),
        };
        if !is_regex && case_sensitive {
            return Ok(Self::Literal(query.to_owned()));
        }
        let pattern = if is_regex {
            query.to_owned()
        } else {
            regex::escape(query)
        };
        let compiled = regex::RegexBuilder::new(&pattern)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|_| Refusal::failed(COMMAND, "invalid regex: pattern could not be compiled"))?;
        Ok(Self::Pattern(compiled))
    }

    /// Visit every match in `text` as a `(scalar offset, scalar length)` pair.
    ///
    /// SCALAR, NOT BYTE AND NOT UTF-16. `text_range_to_columns` walks a row's
    /// spans counting Unicode scalars, so a byte offset would resolve a match
    /// into the wrong cell of any row containing something outside ASCII, and a
    /// UTF-16 offset would do the same for an astral character. The
    /// conversion happens once per hit, rather than by materialising every row
    /// as a `Vec<char>`.
    pub(super) fn visit(&self, text: &str, mut visit: impl FnMut(usize, usize)) {
        match self {
            Self::Literal(needle) => {
                let mut from = 0usize;
                while let Some(found) = text[from..].find(needle.as_str()) {
                    let start = from + found;
                    visit(
                        text[..start].chars().count(),
                        needle.chars().count(),
                    );
                    // Non-overlapping, exactly as v2's `indexOf` loop: an
                    // overlapping match is a second copy of one the caller
                    // already has.
                    from = start + needle.len();
                }
            }
            Self::Pattern(compiled) => {
                for found in compiled.find_iter(text) {
                    visit(
                        text[..found.start()].chars().count(),
                        found.as_str().chars().count(),
                    );
                }
            }
        }
    }
}

/// What one row's scan produced.
pub(super) struct RowScan {
    /// The row as painted, kept because the preview is derived from it once.
    pub(super) text: String,
    pub(super) hits: Vec<Hit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Hit {
    pub(super) col: u32,
    pub(super) len: u32,
}

/// Scan one row, or `None` when the grid no longer holds it.
///
/// A row that vanished between two slices is an ERROR rather than a row to
/// skip: the page reports a half-open scanned range, so a skipped row is a
/// hole in that range the caller cannot see.
pub(super) fn scan_row(
    record: &SessionRecord,
    matcher: &Matcher,
    row: u32,
) -> Option<RowScan> {
    let spans = crate::session::retained_grid::row_spans(record, row)?;
    let text = spans_text(&spans);
    let mut hits = Vec::new();
    matcher.visit(&text, |offset, len| {
        let columns = text_range_to_columns(&spans, offset, len);
        hits.push(Hit {
            col: columns.col,
            len: columns.columns,
        });
    });
    Some(RowScan { text, hits })
}

/// A row's own text, trimmed and bounded — computed ONCE per row.
///
/// This is the global-search preview: a match is only useful to a caller who
/// can see what it was in. The trim drops the trailing pad a grid row always
/// carries, and the bound is the protocol's CODE-POINT cap rather than a byte
/// cap, because the cap exists to keep the reply small, not to split a
/// multi-byte character.
pub(super) fn preview_of(text: &str) -> String {
    text.trim_end()
        .chars()
        .take(TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS)
        .collect()
}
