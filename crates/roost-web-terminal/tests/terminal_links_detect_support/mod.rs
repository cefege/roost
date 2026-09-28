//! Detection fixtures shared by the inferred-link test binaries: narrow rows,
//! a stub worker-file resolver, detection shorthands and UTF-16 slicing.
//! A test binary pulls it in with `mod terminal_links_detect_support;`.
//! Ported from the helpers of `apps/web/tests/renderer/terminal-links.test.ts`.

#![allow(dead_code)]

use roost_web_terminal::links::{PaintedLink, RowLinkInput, RowLinkSegment, compute_row_links};

/// Rows of NARROW characters: one column per UTF-16 code unit.
pub fn rows(texts: &[&str]) -> Vec<RowLinkInput> {
    texts.iter().map(|text| narrow(text, Vec::new())).collect()
}

pub fn narrow(text: &str, links: Vec<PaintedLink>) -> RowLinkInput {
    RowLinkInput {
        text: text.to_string(),
        columns: text.encode_utf16().count() as i64,
        links,
    }
}

pub fn stub_resolve(path: &str, line: Option<u64>, _authority: Option<&str>) -> Option<String> {
    let path = path.strip_prefix('/').unwrap_or(path);
    Some(match line {
        Some(line) => format!("/file/W/{path}#L{line}"),
        None => format!("/file/W/{path}"),
    })
}

pub fn detect(texts: &[&str], cols: i64) -> Vec<RowLinkSegment> {
    compute_row_links(&rows(texts), cols, None, None)
}

pub fn detect_files(texts: &[&str]) -> Vec<RowLinkSegment> {
    compute_row_links(&rows(texts), 80, Some(&stub_resolve), None)
}

pub fn has_file(texts: &[&str]) -> bool {
    detect_files(texts)
        .iter()
        .any(|segment| segment.file.is_some())
}

/// JS `text.slice(start, end)` by UTF-16 code unit.
pub fn slice(text: &str, start: usize, end: usize) -> String {
    String::from_utf16(&text.encode_utf16().collect::<Vec<_>>()[start..end]).unwrap()
}

pub fn reassemble(texts: &[&str], segments: &[RowLinkSegment]) -> String {
    segments
        .iter()
        .map(|segment| slice(texts[segment.row], segment.start, segment.end))
        .collect()
}

pub fn url_segment(row: usize, start: usize, end: usize, url: &str) -> RowLinkSegment {
    RowLinkSegment {
        row,
        start,
        end,
        url: url.to_string(),
        file: None,
    }
}

pub const PAINTED_KEY: &str = "b\u{0}7";
