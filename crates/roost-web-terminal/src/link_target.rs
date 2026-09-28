//! Validates terminal-authored URLs and file targets before any anchor paints or
//! opens. The row painter calls it for every OSC 8 run the core authored, and
//! `links::detect`, `links::anchor` and `links::attachment` call the same
//! authority for inferred and producer-painted links. Worker-aware callers pass
//! the only path-to-route resolver. Parses URLs with the WHATWG `url` crate so
//! the answer is v2's `new URL()`. Ports `apps/web/src/renderer/terminal-links.target.ts`.

use roost_protocol::cell::link_uri_within_cap;
use url::Url;

/// v2 `ResolveFile`: a raw path from terminal output, its optional 1-based
/// line and the `file://host` authority (present only for such a target) →
/// an internal `/file/<workerFp>/…#L<line>` route, or `None` to skip it.
pub type ResolveFile<'a> = &'a dyn Fn(&str, Option<u64>, Option<&str>) -> Option<String>;

/// v2 `Number.MAX_SAFE_INTEGER`: the largest line `Number.isSafeInteger` admits.
const MAX_SAFE_LINE: u64 = (1 << 53) - 1;

/// A classified terminal link target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalLinkTarget {
    /// An absolute HTTP(S) URL: the only target handed to the browser.
    External {
        /// Exactly the terminal-authored text.
        href: String,
        /// The hover text, the same text.
        display: String,
    },
    /// A worker-local file target.
    WorkerFile {
        /// The decoded path, with a trailing `:line[:col]` split off.
        raw_path: String,
        /// The 1-based line, when the target carried one.
        line: Option<u64>,
        /// The `file://host` authority when it named anything but localhost.
        file_authority: Option<String>,
        /// The authenticated route; `None` until a worker-aware resolver ran.
        href: Option<String>,
        /// The exact terminal-authored target.
        display: String,
    },
}

impl TerminalLinkTarget {
    /// The terminal-authored target, which is the hover text of both kinds.
    pub fn display(&self) -> &str {
        match self {
            Self::External { display, .. } | Self::WorkerFile { display, .. } => display,
        }
    }
}

/// Classify untrusted terminal output. External navigation is limited to
/// absolute HTTP(S); a file target resolves to an authenticated worker route
/// when a resolver is given, and carries no href when none is.
pub fn classify_terminal_link_target(
    raw: &str,
    resolve_file: Option<ResolveFile<'_>>,
) -> Option<TerminalLinkTarget> {
    if raw.is_empty()
        || has_js_whitespace_edge(raw)
        || raw.chars().any(|character| character <= ' ' || character == '\u{7f}')
        || !link_uri_within_cap(raw)
    {
        return None;
    }
    if strip_prefix_ignore_ascii_case(raw, "http://").is_some()
        || strip_prefix_ignore_ascii_case(raw, "https://").is_some()
    {
        let url = Url::parse(raw).ok()?;
        let reachable = matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some_and(|host| !host.is_empty());
        return reachable.then(|| TerminalLinkTarget::External {
            href: raw.to_string(),
            display: raw.to_string(),
        });
    }
    if strip_prefix_ignore_ascii_case(raw, "file:").is_some() {
        return classify_file_uri(raw, resolve_file);
    }
    // `//host/share` is ambiguous with a protocol-relative URL. It survives only
    // when the worker-aware resolver accepts it as a Windows UNC path.
    if raw.starts_with("//") {
        let resolve = resolve_file?;
        let (path, line) = split_path_line(raw);
        return resolved_file_target(path, line, raw, Some(resolve), None);
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
    resolved_file_target(path, line, raw, resolve_file, None)
}

/// Only routes the worker file route minted may be installed on an internal
/// terminal anchor: `/file/<workerFp>/<path>` with an optional `#L<line>`.
/// Query strings, other fragments and over-cap routes are refused.
pub fn is_worker_file_href(href: &str) -> bool {
    let Some(rest) = href.strip_prefix("/file/") else {
        return false;
    };
    let Some((worker_fp, tail)) = rest.split_once('/') else {
        return false;
    };
    if worker_fp.is_empty() || worker_fp.contains(['?', '#']) {
        return false;
    }
    let shaped = match tail.split_once('#') {
        None => !tail.is_empty() && !tail.contains('?'),
        Some((path, fragment)) => {
            !path.is_empty()
                && !path.contains('?')
                && fragment
                    .strip_prefix('L')
                    .is_some_and(|digits| is_ascii_digits(digits) && !digits.starts_with('0'))
        }
    };
    shaped && link_uri_within_cap(href)
}

/// v2 `FILE_NAME_RE`: `^[^/\\:]+\.[A-Za-z][\w-]{0,15}(?::\d+(?::\d+)?)?$`.
pub(crate) fn is_explicit_file_name(raw: &str) -> bool {
    let stem_end = raw.find(['/', '\\', ':']).unwrap_or(raw.len());
    if raw[stem_end..].contains(['/', '\\']) {
        return false;
    }
    let (name, suffix) = raw.split_at(stem_end);
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    let mut extension_characters = extension.chars();
    let extension_ok = !stem.is_empty()
        && extension_characters
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic())
        && extension.len() <= 16
        && extension_characters.all(|character| is_word_character(character) || character == '-');
    extension_ok && (suffix.is_empty() || is_line_and_column_suffix(suffix))
}

/// v2 `WINDOWS_DRIVE_ABS_RE`: `^[A-Za-z]:[\\/]`.
pub(crate) fn is_windows_drive_absolute(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

/// JS `\s` (and the set `String.prototype.trim` strips), by UTF-16 code unit.
pub(crate) fn is_js_whitespace(unit: u32) -> bool {
    matches!(
        unit,
        0x09..=0x0d
            | 0x20
            | 0xa0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

/// JS `\w`: ASCII letters, digits and `_`.
pub(crate) fn is_word_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn has_js_whitespace_edge(raw: &str) -> bool {
    let first = raw.chars().next().map(u32::from);
    let last = raw.chars().next_back().map(u32::from);
    first.is_some_and(is_js_whitespace) || last.is_some_and(is_js_whitespace)
}

fn strip_prefix_ignore_ascii_case<'a>(raw: &'a str, prefix: &str) -> Option<&'a str> {
    let head = raw.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &raw[prefix.len()..])
}

fn classify_file_uri(
    raw: &str,
    resolve_file: Option<ResolveFile<'_>>,
) -> Option<TerminalLinkTarget> {
    // Requiring `//` rejects browser-style `file:relative` coercion.
    strip_prefix_ignore_ascii_case(raw, "file://")?;
    let url = Url::parse(raw).ok()?;
    if url.scheme() != "file"
        || !url.username().is_empty()
        || url.password().is_some_and(|password| !password.is_empty())
        || url.query().is_some_and(|query| !query.is_empty())
    {
        return None;
    }
    let fragment_line = match url.fragment().filter(|fragment| !fragment.is_empty()) {
        None => None,
        Some(fragment) => {
            let digits = fragment.strip_prefix('L')?;
            if digits.starts_with('0') {
                return None;
            }
            Some(safe_line(digits)?)
        }
    };
    let mut raw_path = percent_decode(url.path())?;
    // WHATWG file URLs spell a Windows drive as `/C:/path`.
    if raw_path.starts_with('/') && is_windows_drive_absolute(&raw_path[1..]) {
        raw_path.remove(0);
    }
    let (path, line) = match fragment_line {
        Some(line) => (raw_path, Some(line)),
        None => split_path_line(&raw_path),
    };
    let hostname = url.host_str().unwrap_or("");
    let authority = if !hostname.is_empty() && !hostname.eq_ignore_ascii_case("localhost") {
        Some(percent_decode(hostname)?)
    } else {
        None
    };
    resolved_file_target(path, line, raw, resolve_file, authority)
}

fn resolved_file_target(
    raw_path: String,
    line: Option<u64>,
    display: &str,
    resolve_file: Option<ResolveFile<'_>>,
    file_authority: Option<String>,
) -> Option<TerminalLinkTarget> {
    if raw_path.is_empty()
        || raw_path
            .chars()
            .any(|character| character < ' ' || character == '\u{7f}')
    {
        return None;
    }
    let href = match resolve_file {
        None => None,
        Some(resolve) => {
            let href = resolve(&raw_path, line, file_authority.as_deref())?;
            if href.is_empty() || !is_worker_file_href(&href) {
                return None;
            }
            Some(href)
        }
    };
    Some(TerminalLinkTarget::WorkerFile {
        raw_path,
        line,
        file_authority,
        href,
        display: display.to_string(),
    })
}

/// v2 `splitPathLine`: `^(.*?):(\d+)(?::\d+)?$`, then a safe line above zero.
/// The FIRST colon whose tail has that shape decides; a zero or unsafe line
/// there keeps the whole target as the path rather than trying a later colon.
fn split_path_line(raw: &str) -> (String, Option<u64>) {
    for (index, character) in raw.char_indices() {
        if matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            break;
        }
        if character != ':' || !is_line_and_column_suffix(&raw[index..]) {
            continue;
        }
        let tail = &raw[index + 1..];
        let digits = tail.split_once(':').map_or(tail, |(line, _)| line);
        return match safe_line(digits).filter(|line| *line > 0) {
            Some(line) => (raw[..index].to_string(), Some(line)),
            None => (raw.to_string(), None),
        };
    }
    (raw.to_string(), None)
}

/// `:\d+(?::\d+)?` exactly, to the end of the input.
fn is_line_and_column_suffix(suffix: &str) -> bool {
    let Some(tail) = suffix.strip_prefix(':') else {
        return false;
    };
    match tail.split_once(':') {
        Some((line, column)) => is_ascii_digits(line) && is_ascii_digits(column),
        None => is_ascii_digits(tail),
    }
}

/// `Number(digits)` when `Number.isSafeInteger` admits it.
fn safe_line(digits: &str) -> Option<u64> {
    if !is_ascii_digits(digits) {
        return None;
    }
    digits.parse::<u64>().ok().filter(|line| *line <= MAX_SAFE_LINE)
}

/// v2 `URI_SCHEME_RE`: `^[A-Za-z][A-Za-z0-9+.-]*:`.
fn has_uri_scheme(raw: &str) -> bool {
    let Some(colon) = raw.find(':') else {
        return false;
    };
    let mut characters = raw[..colon].chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

/// v2 `WINDOWS_UNC_RE`: `^\\\\[^\\]+\\[^\\]+`.
fn is_windows_unc(raw: &str) -> bool {
    let Some(share) = raw.strip_prefix("\\\\") else {
        return false;
    };
    match share.split_once('\\') {
        Some((host, path)) => !host.is_empty() && !path.is_empty() && !path.starts_with('\\'),
        None => false,
    }
}

fn is_ascii_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// `decodeURIComponent`: refuses a malformed escape or invalid UTF-8.
fn percent_decode(value: &str) -> Option<String> {
    if !value.contains('%') {
        return Some(value.to_string());
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_octet(*bytes.get(index + 1)?)?;
            let low = hex_octet(*bytes.get(index + 2)?)?;
            decoded.push(high << 4 | low);
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
