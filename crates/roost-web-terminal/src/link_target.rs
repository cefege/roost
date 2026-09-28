//! Untrusted terminal-authored link targets, classified before any anchor is
//! painted. The renderer calls this for every OSC 8 run the core authored; the
//! linkifier calls the same authority for inferred matches.
//!
//! Terminal output is hostile input, so the answer is narrow by construction:
//! only an absolute HTTP(S) URL is allowed to leave the app, and a file target
//! carries no browser-openable href at all until a worker-aware resolver turns
//! it into an authenticated in-app route. Nothing here re-derives a link from
//! link TEXT — a painted anchor is a link because the core said so, at the
//! cells the core said.
//!
//! Pure: it parses with hand-written scans rather than a URL library, and every
//! rejection is a refusal to paint, never an approximation.

/// A classified terminal link target, in the two shapes an anchor can take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalLinkTarget {
    /// An absolute HTTP(S) URL. The only target that may be handed to the
    /// browser as an `href`.
    External {
        /// Exactly the terminal-authored text, which is also the display.
        href: String,
        /// The hover hint, the same text.
        display: String,
    },
    /// A worker-local file target. It has no `href` here on purpose: the
    /// activation path resolves it against the current worker and cwd before
    /// installing an authenticated route.
    WorkerFile {
        /// The decoded path, with a trailing `:line[:col]` already split off.
        raw_path: String,
        /// The 1-based line, when the target carried one.
        line: Option<u32>,
        /// The `file://host` authority, when it named anything but localhost.
        file_authority: Option<String>,
        /// The exact terminal-authored target, which is the hover hint.
        display: String,
    },
}

/// Classify one terminal-authored target, or refuse to paint it at all.
///
/// Returns `None` for anything that is not provably a supported target: a
/// custom or rejected scheme, a protocol-relative `//host/share` (ambiguous
/// with a URL until a worker-aware resolver accepts it as a UNC path), and any
/// target that is over the link-URI cap, carries a control character, or has
/// untrimmed surrounding whitespace.
pub fn classify_terminal_link_target(raw: &str) -> Option<TerminalLinkTarget> {
    if is_rejected_everywhere(raw) {
        return None;
    }
    if strip_prefix_ignore_ascii_case(raw, "http://").is_some()
        || strip_prefix_ignore_ascii_case(raw, "https://").is_some()
    {
        return classify_absolute_http(raw);
    }
    if strip_prefix_ignore_ascii_case(raw, "file:").is_some() {
        return classify_file_uri(raw);
    }
    // `//host/share` is ambiguous with a protocol-relative URL, and only a
    // worker-aware resolver can tell it apart, so it never becomes an anchor.
    if raw.starts_with("//") {
        return None;
    }
    let windows_path = is_windows_drive_absolute(raw) || is_windows_unc(raw);
    let explicit_file_name = is_explicit_file_name(raw);
    if !windows_path && !explicit_file_name && has_uri_scheme(raw) {
        return None;
    }
    let path_like = windows_path
        || raw.starts_with('/')
        || raw.starts_with("./")
        || raw.starts_with("../")
        || raw.starts_with("~/")
        || raw.contains('/')
        || raw.contains('\\')
        || explicit_file_name;
    if !path_like {
        return None;
    }
    let (path, line) = split_path_line(raw);
    worker_file_target(path, line, None, raw)
}

/// Targets carrying a space, a control character, surrounding whitespace, or
/// more bytes than the wire allows are refused before any scheme is read: the
/// cap is why a 2 KB URI retargeting a click is not representable at all.
fn is_rejected_everywhere(raw: &str) -> bool {
    raw.is_empty()
        || raw.trim() != raw
        || raw
            .chars()
            .any(|character| character.is_ascii_control() || character == ' ')
        || !roost_protocol::cell::link_uri_within_cap(raw)
}

fn strip_prefix_ignore_ascii_case<'a>(raw: &'a str, prefix: &str) -> Option<&'a str> {
    let head = raw.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &raw[prefix.len()..])
}

fn classify_absolute_http(raw: &str) -> Option<TerminalLinkTarget> {
    let scheme_end = raw.find("//")?;
    let authority_and_path = &raw[scheme_end + 2..];
    let authority_end = authority_and_path
        .find(['/', '?', '#'])
        .unwrap_or(authority_and_path.len());
    if !is_valid_http_authority(&authority_and_path[..authority_end]) {
        return None;
    }
    Some(TerminalLinkTarget::External {
        href: raw.to_string(),
        display: raw.to_string(),
    })
}

/// The authority a browser would actually connect to: a host, an optional port,
/// or a bracketed IPv6 literal. Empty, malformed, or control-carrying hosts are
/// refused rather than handed to the browser to fail on.
fn is_valid_http_authority(authority: &str) -> bool {
    if authority.is_empty() {
        return false;
    }
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let Some(close) = rest.find(']') else {
            return false;
        };
        let literal = &rest[..close];
        let after = &rest[close + 1..];
        let port = match after.strip_prefix(':') {
            Some(port) => Some(port),
            None if after.is_empty() => None,
            None => return false,
        };
        if literal.is_empty()
            || !literal
                .chars()
                .all(|character| character.is_ascii_hexdigit() || matches!(character, ':' | '.'))
        {
            return false;
        }
        // `rest` is `authority` minus its `[`, so the closing bracket sits at
        // `close + 1` in `authority` and the host runs to `close + 2`. Slicing
        // from `rest`'s own length instead ran one byte past the authority and
        // panicked on every bracketed literal with a port.
        (&authority[..close + 2], port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if host.is_empty() || port.is_some_and(|port| !is_ascii_digits(port)) {
        return false;
    }
    if host.starts_with('[') {
        return true;
    }
    !host.chars().any(is_forbidden_host_character)
}

fn is_forbidden_host_character(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            '\0' | '#' | '/' | ':' | '<' | '>' | '?' | '@' | '[' | '\\' | ']' | '^' | '|'
        )
}

fn classify_file_uri(raw: &str) -> Option<TerminalLinkTarget> {
    // Requiring `//` refuses browser-style `file:relative` coercion.
    let rest = strip_prefix_ignore_ascii_case(raw, "file://")?;
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let tail = &rest[authority_end..];
    if authority.contains('@') {
        return None;
    }
    let (path_part, fragment) = match tail.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment)),
        None => (tail, None),
    };
    if path_part.contains('?') {
        return None;
    }
    let fragment_line = match fragment {
        None => None,
        Some(fragment) => Some(parse_line_fragment(fragment)?),
    };
    let mut raw_path = percent_decode(path_part)?;
    // WHATWG file URLs spell a Windows drive as `/C:/path`.
    if let Some(stripped) = raw_path.strip_prefix('/')
        && is_windows_drive_absolute(stripped)
    {
        raw_path = stripped.to_string();
    }
    let (path, line) = match fragment_line {
        Some(line) => (raw_path, Some(line)),
        None => split_path_line(&raw_path),
    };
    let file_authority = if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
        Some(percent_decode(authority)?)
    } else {
        None
    };
    worker_file_target(path, line, file_authority, raw)
}

/// The only fragment a file target may carry: `#L<line>`, a 1-based line with
/// no leading zero. Anything else means the target is not a file view request.
fn parse_line_fragment(fragment: &str) -> Option<u32> {
    let digits = fragment.strip_prefix('L')?;
    if digits.is_empty() || digits.starts_with('0') || !is_ascii_digits(digits) || digits.len() > 9
    {
        return None;
    }
    digits.parse().ok()
}

/// Split a trailing `:line[:col]` off a file candidate. The viewer has a line
/// contract and no column contract, so a valid column is consumed and dropped.
fn split_path_line(raw: &str) -> (String, Option<u32>) {
    for (index, character) in raw.char_indices() {
        if character != ':' {
            continue;
        }
        if let Some(line) = parse_line_and_optional_column(&raw[index + 1..]) {
            return (raw[..index].to_string(), Some(line));
        }
    }
    (raw.to_string(), None)
}

fn parse_line_and_optional_column(tail: &str) -> Option<u32> {
    let (line, column) = match tail.find(':') {
        Some(cut) => (&tail[..cut], Some(&tail[cut + 1..])),
        None => (tail, None),
    };
    if line.is_empty() || !is_ascii_digits(line) {
        return None;
    }
    if column.is_some_and(|column| column.is_empty() || !is_ascii_digits(column)) {
        return None;
    }
    let line: u32 = line.parse().ok()?;
    (line > 0).then_some(line)
}

fn worker_file_target(
    raw_path: String,
    line: Option<u32>,
    file_authority: Option<String>,
    display: &str,
) -> Option<TerminalLinkTarget> {
    if raw_path.is_empty()
        || raw_path
            .chars()
            .any(|character| character.is_ascii_control())
    {
        return None;
    }
    Some(TerminalLinkTarget::WorkerFile {
        raw_path,
        line,
        file_authority,
        display: display.to_string(),
    })
}

fn has_uri_scheme(raw: &str) -> bool {
    let Some(colon) = raw.find(':') else {
        return false;
    };
    let scheme = &raw[..colon];
    let mut characters = scheme.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

fn is_windows_drive_absolute(raw: &str) -> bool {
    let mut characters = raw.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        && matches!(characters.next(), Some(':'))
        && matches!(characters.next(), Some('/') | Some('\\'))
}

fn is_windows_unc(raw: &str) -> bool {
    let Some(share) = raw.strip_prefix("\\\\") else {
        return false;
    };
    match share.split_once('\\') {
        Some((host, path)) => !host.is_empty() && !path.is_empty(),
        None => false,
    }
}

/// A bare `name.ext`, `name.ext:line` or `name.ext:line:col` with no directory
/// separator: the one path-like shape that is safe to linkify from prose.
fn is_explicit_file_name(raw: &str) -> bool {
    // The line suffix is part of the shape, not a separate test: a `:12` that
    // does not parse stays inside the name, which then contains a colon and is
    // no longer a bare file name.
    let (stem, _line) = split_path_line(raw);
    if stem.contains(['/', '\\', ':']) {
        return false;
    }
    let Some((name, extension)) = stem.rsplit_once('.') else {
        return false;
    };
    if name.is_empty() {
        return false;
    }
    let mut characters = extension.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        && characters.by_ref().take(15).all(is_word_character)
        && characters.next().is_none()
}

fn is_ascii_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_word_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
}

/// Percent-decode, refusing a malformed escape or an invalid UTF-8 sequence:
/// a target that cannot be decoded is a target nobody vouched for.
fn percent_decode(value: &str) -> Option<String> {
    if !value.contains('%') {
        return Some(value.to_string());
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push(hex_octet(high)? << 4 | hex_octet(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_octet(character: u8) -> Option<u8> {
    match character {
        b'0'..=b'9' => Some(character - b'0'),
        b'a'..=b'f' => Some(character - b'a' + 10),
        b'A'..=b'F' => Some(character - b'A' + 10),
        _ => None,
    }
}
