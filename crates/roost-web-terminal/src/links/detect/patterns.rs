//! The URL-family patterns of the inferred-link detector: explicit URI schemes,
//! scheme tokens without `//`, scheme-less `localhost:PORT` dev URLs and GitHub
//! refs. Each scanner reproduces its v2 JavaScript regex exactly — leftmost
//! match, greedy with backtracking, `\w`/`\s`/`\b` by UTF-16 code unit — over
//! the joined logical line `links::detect` builds. Ports the pattern sources in
//! `apps/web/src/renderer/terminal-links.detect.ts`.

use crate::link_target::is_js_whitespace;

/// One code unit of the line, or `None` past its end.
pub(super) fn unit_at(text: &[u16], index: usize) -> Option<u16> {
    text.get(index).copied()
}

pub(super) fn is_ascii(unit: u16, predicate: fn(&u8) -> bool) -> bool {
    u8::try_from(unit).is_ok_and(|byte| predicate(&byte))
}

fn is_byte(unit: Option<u16>, byte: u8) -> bool {
    unit == Some(u16::from(byte))
}

/// JS `\w`.
pub(super) fn is_word(unit: u16) -> bool {
    is_ascii(unit, u8::is_ascii_alphanumeric) || unit == u16::from(b'_')
}

fn is_one_of(unit: u16, set: &[u8]) -> bool {
    u8::try_from(unit).is_ok_and(|byte| set.contains(&byte))
}

/// `SCHEME_URL_CHARS`: `[\w\-.~:/?#@!$&*+,;=%]`.
fn is_scheme_url_char(unit: u16) -> bool {
    is_word(unit) || is_one_of(unit, b"-.~:/?#@!$&*+,;=%")
}

/// `LINK_CHAR_RE`: one backslash wider than the URL body, so a native Windows
/// path is not trimmed at a soft-wrap boundary.
pub(super) fn is_link_char(unit: u16) -> bool {
    is_scheme_url_char(unit) || unit == u16::from(b'\\')
}

/// `[A-Za-z0-9+.-]`, the scheme body.
fn is_scheme_char(unit: u16) -> bool {
    is_ascii(unit, u8::is_ascii_alphanumeric) || is_one_of(unit, b"+.-")
}

/// `[A-Za-z0-9_.-]`, a GitHub owner or repo name.
fn is_repo_char(unit: u16) -> bool {
    is_word(unit) || is_one_of(unit, b".-")
}

pub(super) fn is_lower_hex(unit: u16) -> bool {
    is_ascii(unit, u8::is_ascii_digit) || (u16::from(b'a')..=u16::from(b'f')).contains(&unit)
}

pub(super) fn run_end(text: &[u16], from: usize, member: fn(u16) -> bool) -> usize {
    let mut end = from;
    while unit_at(text, end).is_some_and(member) {
        end += 1;
    }
    end
}

pub(super) fn starts_with_ascii(text: &[u16], at: usize, literal: &[u8]) -> bool {
    text.len() >= at + literal.len()
        && text[at..at + literal.len()]
            .iter()
            .zip(literal)
            .all(|(unit, byte)| *unit == u16::from(*byte))
}

/// The `\d+` run at `from`, or `None` when no digit is there.
pub(super) fn digits_end(text: &[u16], from: usize) -> Option<usize> {
    let end = run_end(text, from, |unit| is_ascii(unit, u8::is_ascii_digit));
    (end > from).then_some(end)
}

/// JS `\b` at `index`.
pub(super) fn is_word_boundary(text: &[u16], index: usize) -> bool {
    let before = index > 0 && is_word(text[index - 1]);
    let after = unit_at(text, index).is_some_and(is_word);
    before != after
}

const BARE_URI_SCHEMES: [&[u8]; 7] = [
    b"mailto",
    b"javascript",
    b"data",
    b"vbscript",
    b"tel",
    b"magnet",
    b"news",
];

/// The next `URL_RE_SOURCE` match at or after `from`.
pub(super) fn next_scheme_url(text: &[u16], from: usize) -> Option<(usize, usize)> {
    // A scheme run ends where `[A-Za-z0-9+.-]*` stops, and so does every scheme
    // starting inside it: one "no `://` here" answer covers the whole run.
    let mut run_without_separator = 0;
    for start in from..text.len() {
        if start >= run_without_separator && is_ascii(text[start], u8::is_ascii_alphabetic) {
            let scheme_end = run_end(text, start + 1, is_scheme_char);
            if starts_with_ascii(text, scheme_end, b"://") {
                if let Some(end) = scheme_url_body(text, scheme_end + 3) {
                    return Some((start, end));
                }
            } else {
                run_without_separator = scheme_end;
            }
        }
        for scheme in BARE_URI_SCHEMES {
            if starts_with_ascii(text, start, scheme)
                && is_byte(unit_at(text, start + scheme.len()), b':')
                && let Some(end) = scheme_url_body(text, start + scheme.len() + 1)
            {
                return Some((start, end));
            }
        }
    }
    None
}

/// `(?:IPV6_BODY|SCHEME_URL_CHARS+BRACKETED_SUFFIX)+(?<![,.])` at `body`.
fn scheme_url_body(text: &[u16], body: usize) -> Option<usize> {
    let mut position = body;
    loop {
        if let Some(end) = ipv6_body(text, position) {
            position = end;
            continue;
        }
        let chars_end = run_end(text, position, is_scheme_url_char);
        if chars_end == position {
            break;
        }
        position = bracketed_suffix(text, chars_end).unwrap_or(chars_end);
    }
    // The lookbehind only ever rejects a trailing `,` or `.`, and both come from
    // a character run, where every shorter end is a reachable backtrack.
    while position > body && matches!(u8::try_from(text[position - 1]), Ok(b',' | b'.')) {
        position -= 1;
    }
    (position > body).then_some(position)
}

/// `\[[0-9a-fA-F:]+\](?::\d+)?`.
fn ipv6_body(text: &[u16], at: usize) -> Option<usize> {
    if !is_byte(unit_at(text, at), b'[') {
        return None;
    }
    let literal_end = run_end(text, at + 1, |unit| {
        is_ascii(unit, u8::is_ascii_hexdigit) || unit == u16::from(b':')
    });
    if literal_end == at + 1 || !is_byte(unit_at(text, literal_end), b']') {
        return None;
    }
    let close = literal_end + 1;
    if is_byte(unit_at(text, close), b':')
        && let Some(port_end) = digits_end(text, close + 1)
    {
        return Some(port_end);
    }
    Some(close)
}

/// `(?:[\(\[]\w*[\)\]])` — keeps a matching `)` inside the URL.
fn bracketed_suffix(text: &[u16], at: usize) -> Option<usize> {
    if !unit_at(text, at).is_some_and(|unit| is_one_of(unit, b"([")) {
        return None;
    }
    let word_end = run_end(text, at + 1, is_word);
    unit_at(text, word_end)
        .is_some_and(|unit| is_one_of(unit, b")]"))
        .then_some(word_end + 1)
}

/// The next `(?<![\w.])[A-Za-z][A-Za-z0-9+.-]*:[^\s]*` match at or after `from`.
pub(super) fn next_scheme_token(text: &[u16], from: usize) -> Option<(usize, usize)> {
    for start in from..text.len() {
        let boundary =
            start == 0 || !(is_word(text[start - 1]) || text[start - 1] == u16::from(b'.'));
        if !boundary || !is_ascii(text[start], u8::is_ascii_alphabetic) {
            continue;
        }
        let scheme_end = run_end(text, start + 1, is_scheme_char);
        if is_byte(unit_at(text, scheme_end), b':') {
            let end = run_end(text, scheme_end + 1, |unit| {
                !is_js_whitespace(u32::from(unit))
            });
            return Some((start, end));
        }
    }
    None
}

const LOOPBACK_HOSTS: [&[u8]; 3] = [b"localhost", b"127.0.0.1", b"0.0.0.0"];

/// Whether `raw` starts `^(?:localhost|127\.0\.0\.1|0\.0\.0\.0):\d+(?:\/|$)`.
pub(super) fn is_loopback_origin(raw: &[u16]) -> bool {
    LOOPBACK_HOSTS.iter().any(|host| {
        starts_with_ascii(raw, 0, host)
            && is_byte(unit_at(raw, host.len()), b':')
            && digits_end(raw, host.len() + 1)
                .is_some_and(|end| end == raw.len() || is_byte(unit_at(raw, end), b'/'))
    })
}

/// The next `LOCALHOST_RE_SOURCE` match at or after `from`.
pub(super) fn next_loopback_url(text: &[u16], from: usize) -> Option<(usize, usize)> {
    for start in from..text.len() {
        for host in LOOPBACK_HOSTS {
            if !starts_with_ascii(text, start, host)
                || !is_byte(unit_at(text, start + host.len()), b':')
            {
                continue;
            }
            let Some(port_end) = digits_end(text, start + host.len() + 1) else {
                continue;
            };
            if !is_byte(unit_at(text, port_end), b'/') {
                return Some((start, port_end));
            }
            let path_end = run_end(text, port_end + 1, |unit| {
                is_word(unit) || is_one_of(unit, b"-./?#@!$&*+,;=%")
            });
            return Some((start, path_end));
        }
    }
    None
}

/// One `owner/repo<separator><reference>` GitHub ref: the match and its groups.
pub(super) struct RepoRef {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) owner: (usize, usize),
    pub(super) repo: (usize, usize),
    pub(super) reference: (usize, usize),
}

/// The next `owner/repo#N` (`separator` `#`) or `owner/repo@sha` (`@`) match.
pub(super) fn next_repo_ref(text: &[u16], from: usize, separator: u8) -> Option<RepoRef> {
    let mut start = from;
    while start < text.len() {
        if !is_repo_char(text[start]) {
            start += 1;
            continue;
        }
        let owner_end = run_end(text, start, is_repo_char);
        if let Some(found) = repo_ref_after_owner(text, (start, owner_end), separator) {
            return Some(found);
        }
        // Every start inside the owner run shares its continuation, so none of
        // them can match either.
        start = owner_end;
    }
    None
}

fn repo_ref_after_owner(text: &[u16], owner: (usize, usize), separator: u8) -> Option<RepoRef> {
    if !is_byte(unit_at(text, owner.1), b'/') {
        return None;
    }
    let repo = (owner.1 + 1, run_end(text, owner.1 + 1, is_repo_char));
    if repo.0 == repo.1 || !is_byte(unit_at(text, repo.1), separator) {
        return None;
    }
    let reference_start = repo.1 + 1;
    let reference_end = if separator == b'#' {
        digits_end(text, reference_start)?
    } else {
        let hex_end = run_end(text, reference_start, is_lower_hex);
        let length = hex_end - reference_start;
        if !(7..=40).contains(&length) || !is_word_boundary(text, hex_end) {
            return None;
        }
        hex_end
    };
    Some(RepoRef {
        start: owner.0,
        end: reference_end,
        owner,
        repo,
        reference: (reference_start, reference_end),
    })
}

/// The next `(?<![\w/#])#(\d+)\b` match: `(match, digits)`.
pub(super) fn next_bare_issue(
    text: &[u16],
    from: usize,
) -> Option<((usize, usize), (usize, usize))> {
    for start in from..text.len() {
        if text[start] != u16::from(b'#') {
            continue;
        }
        if start > 0 && (is_word(text[start - 1]) || is_one_of(text[start - 1], b"/#")) {
            continue;
        }
        if let Some(end) = digits_end(text, start + 1)
            && is_word_boundary(text, end)
        {
            return Some(((start, end), (start + 1, end)));
        }
    }
    None
}

/// The next `(?<![\w/])(?=[0-9a-f]*[a-f])[0-9a-f]{7,40}(?![\w])` match.
pub(super) fn next_bare_sha(text: &[u16], from: usize) -> Option<(usize, usize)> {
    for start in from..text.len() {
        if !is_lower_hex(text[start]) {
            continue;
        }
        if start > 0 && (is_word(text[start - 1]) || text[start - 1] == u16::from(b'/')) {
            continue;
        }
        let end = run_end(text, start, is_lower_hex);
        let has_letter = text[start..end]
            .iter()
            .any(|unit| !is_ascii(*unit, u8::is_ascii_digit));
        if has_letter
            && (7..=40).contains(&(end - start))
            && !unit_at(text, end).is_some_and(is_word)
        {
            return Some((start, end));
        }
    }
    None
}
