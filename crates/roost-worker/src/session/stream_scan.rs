//! Pure terminal byte-stream scanners: alt-screen transitions, OSC 7 cwd, and
//! the OSC title/progress agent detection falls back to. The only caller is
//! `super::scrollback`; nothing here reads a session, writes a session, or
//! touches a core. Depends on nothing but the standard library — and on nothing
//! that depends on it back.
//!
//! They are separate from the append policy because they are the one part of it
//! with no state: a scanner is a function of the carried prefix plus the new
//! bytes, which is what makes them safe to run against a FROZEN core. The
//! capture lane parses the same stream with the grid held still, so a scanner
//! that needed a live core could not run there at all.
//!
//! EVERY SCANNER TAKES A COMBINED BUFFER — the record's carry, then this
//! chunk — and returns the tail the next chunk must be prefixed with. A
//! sequence split at a chunk boundary is therefore recognised exactly once, and
//! the cap on each carry is what stops an unterminated sequence from pinning
//! memory.
//!
//! WINDOWS IS NOT PORTED. v2's OSC 7 parser reconstructed `C:/…` and
//! `//server/share` paths from drive and UNC emissions; a POSIX worker's
//! `file://` payload is already the path, so `parse_osc7_worker_path` returns it
//! percent-decoded and nothing more.

/// The longest tail of a split DEC private mode sequence kept for the next
/// chunk.
///
/// The longest sequence scanned for is 8 bytes (`ESC [ ? 1 0 4 9 h`), so seven
/// carried bytes is one more than any legitimate match needs.
pub const MODE_CARRY_MAX: usize = 7;

/// The longest OSC 7 payload kept across a chunk boundary.
///
/// A whole sequence is `ESC ] 7 ; file://<host>/<percent-encoded-path> BEL`, and
/// paths beyond a kilobyte are noise rather than a folder.
pub const OSC7_CARRY_MAX: usize = 1024;

/// The longest unterminated OSC 0/2 or OSC 9 body kept for the next chunk.
pub const AGENT_OSC_CARRY_MAX: usize = 1024;

const OSC7_PREFIX: &[u8] = b"\x1b]7;";

/// The alt-screen ENTER sequences, in the order a core is nudged onto the
/// alternate screen during an adoption replay.
///
/// Exported because the replay must put a replacement core where the old one
/// was, and it does that by writing the same bytes the stream carried rather
/// than inventing a variant of them.
pub const ALT_ENTER_SEQUENCES: [&[u8]; 3] = [b"\x1b[?1049h", b"\x1b[?47h", b"\x1b[?1047h"];

/// Every alt-screen toggle is `ESC [ ? N { h | l }` for `N` in 47, 1047 and
/// 1049: 1049 is the modern variant and saves the cursor, 47 is legacy, and
/// 1047 is between them. A TUI on any of them owns the screen, and a rebuilt
/// core that re-entered the wrong one paints redraws onto the wrong rows.
const ALT_LEAVE_SEQUENCES: [&[u8]; 3] = [b"\x1b[?1049l", b"\x1b[?47l", b"\x1b[?1047l"];

/// The LAST toggle in a buffer wins, and a buffer with no toggle leaves the
/// previous answer alone — a chunk that carries no transition must not be
/// allowed to clear a mode a TUI entered a moment ago.
pub fn scan_alt_mode(combined: &[u8], was_alt: bool) -> bool {
    let mut alt = was_alt;
    let mut cursor = 0;
    while cursor < combined.len() {
        let mut next: Option<(usize, bool)> = None;
        for (entering, table) in [(true, &ALT_ENTER_SEQUENCES), (false, &ALT_LEAVE_SEQUENCES)] {
            for sequence in table {
                let Some(at) = find(combined, sequence, cursor) else {
                    continue;
                };
                if next.is_none_or(|(earliest, _)| at < earliest) {
                    next = Some((at, entering));
                }
            }
        }
        let Some((at, entering)) = next else {
            break;
        };
        alt = entering;
        cursor = at + 1;
    }
    alt
}

/// One OSC 7 scan's result: the folder it named, and the tail to re-scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Osc7Scan {
    /// The LAST complete path in the buffer. Several `cd`s in one chunk collapse
    /// to the final destination, because that is where the shell now is.
    pub cwd: Option<String>,
    /// The unparsed tail, at most [`OSC7_CARRY_MAX`] bytes.
    pub carry: Vec<u8>,
}

/// Scan `combined` for `file://` cwd reports.
///
/// A `BEL` or `ST` terminates a sequence; an `ESC` that is not `ST` abandons it,
/// because a payload with a stray `ESC` in it is a sequence something else
/// interrupted and not a folder.
pub fn scan_osc7(combined: &[u8]) -> Osc7Scan {
    let mut cursor = 0;
    let mut cwd: Option<String> = None;
    while cursor < combined.len() {
        let Some(start) = find(combined, OSC7_PREFIX, cursor) else {
            let carry_from = cursor.max(combined.len().saturating_sub(OSC7_PREFIX.len() - 1));
            return Osc7Scan {
                cwd,
                carry: combined[carry_from..].to_vec(),
            };
        };
        let body = start + OSC7_PREFIX.len();
        let Some((terminator, width)) = terminator_after(combined, body) else {
            let carry_from = if combined.len() - start > OSC7_CARRY_MAX {
                combined.len() - OSC7_CARRY_MAX
            } else {
                start
            };
            return Osc7Scan {
                cwd,
                carry: combined[carry_from..].to_vec(),
            };
        };
        // Custom prompts emit raw UTF-8 far more often than they percent-encode
        // a non-ASCII directory name, so the payload is lossy-decoded rather
        // than required to be valid UTF-8.
        let payload = String::from_utf8_lossy(&combined[body..terminator]);
        if let Some(path) = parse_osc7_worker_path(&payload) {
            cwd = Some(path);
        }
        cursor = terminator + width;
    }
    Osc7Scan {
        cwd,
        carry: Vec::new(),
    }
}

/// One OSC title/progress scan's result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentOscScan {
    /// The last complete OSC 0/2 title, stripped of control bytes.
    pub title: Option<String>,
    /// The last complete OSC 9 `4;…` progress body.
    pub progress: Option<String>,
    /// The unparsed tail, at most [`AGENT_OSC_CARRY_MAX`] bytes.
    pub carry: Vec<u8>,
}

/// The most title text a session retains, in UTF-16 code units.
///
/// Capped in code units rather than scalars because the value is user-visible
/// text a front end truncates again for display, and a half-cut surrogate pair
/// is a `U+FFFD` in a window title.
pub const AGENT_OSC_TITLE_MAX: usize = 256;

/// The most progress text a session retains, in the same unit.
pub const AGENT_OSC_PROGRESS_MAX: usize = 64;

/// Scan `combined` for the OSC 0/2 title and OSC 9 `4;` progress bodies.
///
/// Every other OSC code is skipped without consuming its body, so an unrelated
/// OSC cannot swallow a title that follows it in the same chunk.
pub fn scan_agent_osc(combined: &[u8]) -> AgentOscScan {
    let mut scan = AgentOscScan::default();
    let mut cursor = 0;
    while cursor < combined.len() {
        let Some(start) = find(combined, b"\x1b]", cursor) else {
            break;
        };
        let body = start + 2;
        let Some((terminator, width)) = first_terminator(combined, body) else {
            let carry_from = start.max(combined.len().saturating_sub(AGENT_OSC_CARRY_MAX));
            scan.carry = combined[carry_from..].to_vec();
            return scan;
        };
        let payload = String::from_utf8_lossy(&combined[body..terminator]);
        let (code, value) = match payload.split_once(';') {
            Some((code, value)) => (code, value),
            None => (payload.as_ref(), ""),
        };
        match code {
            "0" | "2" => scan.title = Some(take_text(value, AGENT_OSC_TITLE_MAX)),
            "9" => {
                if let Some(progress) = value.strip_prefix("4;") {
                    scan.progress = Some(take_text(progress, AGENT_OSC_PROGRESS_MAX));
                }
            }
            _ => {}
        }
        cursor = terminator + width;
    }
    // A lone trailing ESC is the only prefix of a sequence the next chunk can
    // complete, and keeping it costs one byte; keeping any longer tail would
    // mean re-reporting a title that was already reported.
    if combined.last() == Some(&0x1b) {
        scan.carry = vec![0x1b];
    }
    scan
}

/// A body with its control bytes removed and at most `max` code units kept.
fn take_text(value: &str, max: usize) -> String {
    let mut kept = String::with_capacity(value.len().min(max));
    for character in value.chars().filter(|c| {
        let code = *c as u32;
        !(code < 0x20 || code == 0x7f)
    }) {
        if kept.len() + character.len_utf16() > max {
            break;
        }
        kept.push(character);
    }
    kept
}

/// The worker's canonical cwd for an OSC 7 payload, or `None` when the payload
/// is not a `file://` URL this worker can hold.
///
/// A component that is not valid percent-encoding is returned as it arrived
/// rather than half-decoded, because a partially decoded path names a folder
/// that does not exist.
pub fn parse_osc7_worker_path(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let host = decode_path_component(&rest[..slash]);
    let path = decode_path_component(&rest[slash..]);
    if host.contains('\0') || path.contains('\0') {
        return None;
    }
    Some(path)
}

/// Percent-decode one URL component, whole or not at all.
fn decode_path_component(value: &str) -> String {
    if !value.contains('%') {
        return value.to_owned();
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'%' {
            decoded.push(bytes[at]);
            at += 1;
            continue;
        }
        match (
            bytes.get(at + 1).copied().and_then(hex_digit),
            bytes.get(at + 2).copied().and_then(hex_digit),
        ) {
            (Some(high), Some(low)) => {
                decoded.push((high << 4) | low);
                at += 3;
            }
            _ => return value.to_owned(),
        }
    }
    String::from_utf8(decoded).unwrap_or_else(|_| value.to_owned())
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The first `BEL`, or the first `ESC \`, at or after `from`, with its width.
///
/// An `ESC` that is not `ST` ends the search with no terminator. That is the
/// right answer for a CWD and the wrong one for a title, which is why there are
/// two of these: a folder payload interrupted by a stray escape is a sequence
/// something else broke, and reporting the half of it we saw would be a guess,
/// while a title routinely contains a CSI and stopping at it would mean a
/// prompt that set its window title never got one.
fn terminator_after(combined: &[u8], from: usize) -> Option<(usize, usize)> {
    for at in from..combined.len() {
        match combined[at] {
            0x07 => return Some((at, 1)),
            0x1b => {
                return (combined.get(at + 1) == Some(&b'\\')).then_some((at, 2));
            }
            _ => {}
        }
    }
    None
}

/// The first `BEL` or `ESC \` at or after `from`, whichever comes first, with a
/// bare `ESC` passing over rather than ending the search.
fn first_terminator(combined: &[u8], from: usize) -> Option<(usize, usize)> {
    for at in from..combined.len() {
        match combined[at] {
            0x07 => return Some((at, 1)),
            0x1b if combined.get(at + 1) == Some(&b'\\') => return Some((at, 2)),
            _ => {}
        }
    }
    None
}

/// The first index at or after `from` where `needle` occurs.
///
/// The window runs to the END of the haystack, not to `len - needle.len()`:
/// clipping there drops the last legal match, and the last toggle in a chunk is
/// the one that decides which screen the bytes after it belong to.
fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len().saturating_sub(from) {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}
