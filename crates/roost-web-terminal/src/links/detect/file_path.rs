//! The file-path pattern of the inferred-link detector: Windows drive, UNC,
//! POSIX/home/relative paths with a separator, bare `name.ext:line`, and bare
//! archive names. Reproduces v2's `FILE_RE_SOURCE` alternation exactly —
//! leftmost start, alternatives in order, greedy directory runs backtracked one
//! component at a time — so the resolver receives the same candidate v2 did.
//! Ports `FILE_RE_SOURCE` in `apps/web/src/renderer/terminal-links.detect.ts`.

use super::patterns::{
    digits_end, is_ascii, is_word, is_word_boundary, run_end, starts_with_ascii, unit_at,
};

/// `[\w.@\-]`, one path component character.
fn is_path_part(unit: u16) -> bool {
    is_word(unit) || b".@-".iter().any(|byte| unit == u16::from(*byte))
}

/// `[/\\]`.
fn is_path_separator(unit: Option<u16>) -> bool {
    unit.is_some_and(|unit| unit == u16::from(b'/') || unit == u16::from(b'\\'))
}

fn is_dot(unit: Option<u16>) -> bool {
    unit == Some(u16::from(b'.'))
}

/// Distinctive archive extensions, in v2's alternation order.
const ARCHIVE_EXTENSIONS: [&[u8]; 21] = [
    b"tar.gz",
    b"tar.bz2",
    b"tar.xz",
    b"tar.zst",
    b"tar.lz",
    b"tar.lz4",
    b"tar.lzma",
    b"tar.Z",
    b"tgz",
    b"tbz2",
    b"txz",
    b"zip",
    b"7z",
    b"rar",
    b"dmg",
    b"gz",
    b"bz2",
    b"xz",
    b"zst",
    b"lz4",
    b"tar",
];

/// The next `FILE_RE_SOURCE` match at or after `from`.
pub(super) fn next_file_path(text: &[u16], from: usize) -> Option<(usize, usize)> {
    (from..text.len()).find_map(|start| {
        drive_path(text, start)
            .or_else(|| unc_path(text, start))
            .or_else(|| separated_path(text, start))
            .or_else(|| file_name_with_line(text, start))
            .or_else(|| archive_name(text, start))
            .map(|end| (start, end))
    })
}

/// The starts of each `(?:PART[/\\])` iteration a greedy `*` takes from `at`,
/// `at` itself first: iteration `k` hands `FILE_PART` the position `starts[k]`.
fn component_starts(text: &[u16], at: usize) -> Vec<usize> {
    let mut starts = vec![at];
    let mut position = at;
    loop {
        let part_end = run_end(text, position, is_path_part);
        if part_end == position || !is_path_separator(unit_at(text, part_end)) {
            return starts;
        }
        position = part_end + 1;
        starts.push(position);
    }
}

/// `(?:PART[/\\]){minimum,} FILE_PART FILE_LINE`, backtracking the directory
/// count from the greedy maximum down.
fn directories_then_file(text: &[u16], at: usize, minimum: usize) -> Option<usize> {
    let starts = component_starts(text, at);
    starts
        .iter()
        .skip(minimum)
        .rev()
        .find_map(|start| file_part(text, *start))
        .map(|end| file_line(text, end))
}

/// `[\w.@\-]+\.[A-Za-z][\w-]{0,9}`: the rightmost dot the stem can give back to
/// with a letter after it wins, and the extension is cut at ten characters.
fn file_part(text: &[u16], at: usize) -> Option<usize> {
    let part_end = run_end(text, at, is_path_part);
    (at + 1..part_end).rev().find_map(|dot| {
        if !is_dot(unit_at(text, dot))
            || !unit_at(text, dot + 1).is_some_and(|unit| is_ascii(unit, u8::is_ascii_alphabetic))
        {
            return None;
        }
        let tail_end = run_end(text, dot + 2, |unit| {
            is_word(unit) || unit == u16::from(b'-')
        });
        Some((dot + 2) + (tail_end - (dot + 2)).min(9))
    })
}

/// `(?::\d+(?::\d+)?)?`.
fn file_line(text: &[u16], at: usize) -> usize {
    let colon_digits = |position: usize| {
        (unit_at(text, position) == Some(u16::from(b':')))
            .then(|| digits_end(text, position + 1))
            .flatten()
    };
    match colon_digits(at) {
        None => at,
        Some(line_end) => colon_digits(line_end).unwrap_or(line_end),
    }
}

/// A) `[A-Za-z]:[/\\](?:PART[/\\])*FILE_PART FILE_LINE`.
fn drive_path(text: &[u16], start: usize) -> Option<usize> {
    let drive = unit_at(text, start).is_some_and(|unit| is_ascii(unit, u8::is_ascii_alphabetic))
        && unit_at(text, start + 1) == Some(u16::from(b':'))
        && is_path_separator(unit_at(text, start + 2));
    drive
        .then(|| directories_then_file(text, start + 3, 0))
        .flatten()
}

/// B) `(?:/{2}|\\{2})PART[/\\]PART[/\\](?:PART[/\\])*FILE_PART FILE_LINE`.
fn unc_path(text: &[u16], start: usize) -> Option<usize> {
    if !starts_with_ascii(text, start, b"//") && !starts_with_ascii(text, start, b"\\\\") {
        return None;
    }
    let starts = component_starts(text, start + 2);
    let share = *starts.get(2)?;
    directories_then_file(text, share, 0)
}

/// C) `(?:~[/\\]|\.{0,2}[/\\])?(?:PART[/\\])+FILE_PART FILE_LINE`.
fn separated_path(text: &[u16], start: usize) -> Option<usize> {
    if unit_at(text, start) == Some(u16::from(b'~'))
        && is_path_separator(unit_at(text, start + 1))
        && let Some(end) = directories_then_file(text, start + 2, 1)
    {
        return Some(end);
    }
    let dots = (0..2)
        .take_while(|offset| is_dot(unit_at(text, start + offset)))
        .count();
    for dot_count in (0..=dots).rev() {
        if is_path_separator(unit_at(text, start + dot_count))
            && let Some(end) = directories_then_file(text, start + dot_count + 1, 1)
        {
            return Some(end);
        }
    }
    directories_then_file(text, start, 1)
}

/// D) `FILE_PART:\d+(?::\d+)?` — the extension must end exactly at the colon.
fn file_name_with_line(text: &[u16], start: usize) -> Option<usize> {
    let part_end = run_end(text, start, is_path_part);
    if unit_at(text, part_end) != Some(u16::from(b':')) {
        return None;
    }
    let line_end = digits_end(text, part_end + 1)?;
    let dot = (start + 1..part_end)
        .rev()
        .find(|dot| is_dot(unit_at(text, *dot)))?;
    let extension = &text[dot + 1..part_end];
    let shaped = extension
        .first()
        .is_some_and(|unit| is_ascii(*unit, u8::is_ascii_alphabetic))
        && extension.len() <= 10
        && extension[1..]
            .iter()
            .all(|unit| is_word(*unit) || *unit == u16::from(b'-'));
    shaped.then(|| {
        let column = (unit_at(text, line_end) == Some(u16::from(b':')))
            .then(|| digits_end(text, line_end + 1))
            .flatten();
        column.unwrap_or(line_end)
    })
}

/// E) `PART ARCHIVE_EXT \b`: the stem gives back from the right, and the first
/// extension in order that ends on a word boundary wins.
fn archive_name(text: &[u16], start: usize) -> Option<usize> {
    let part_end = run_end(text, start, is_path_part);
    (start + 1..part_end).rev().find_map(|dot| {
        if !is_dot(unit_at(text, dot)) {
            return None;
        }
        ARCHIVE_EXTENSIONS.iter().find_map(|extension| {
            let end = dot + 1 + extension.len();
            (starts_with_ascii(text, dot + 1, extension) && is_word_boundary(text, end))
                .then_some(end)
        })
    })
}
