//! Pure terminal byte-stream scanners for alt-screen transitions and OSC 7 cwd.
//! The only caller is `super::scrollback`; nothing here reads or writes a
//! session or touches a core. Depends only on the standard library.
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
//! Ports v2 `apps/worker/src/session/session-constants.ts`, `apps/worker/src/terminal/terminal-stream-scan.ts`, `apps/worker/src/util/path.ts`.

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

/// The worker's canonical cwd for an OSC 7 payload, or `None` when the payload
/// is not a `file://` URL this worker can hold.
///
/// A component that is not valid percent-encoding is returned as it arrived
/// rather than half-decoded, because a partially decoded path names a folder
/// that does not exist. On Windows a `file:///C:/…` URL becomes `C:\…`.
pub fn parse_osc7_worker_path(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let host = decode_path_component(&rest[..slash]);
    let path = decode_path_component(&rest[slash..]);
    if host.contains('\0') || path.contains('\0') {
        return None;
    }
    #[cfg(windows)]
    let path = windows_drive_path(&path).unwrap_or(path);
    Some(path)
}

/// `/C:/Users/x` → `C:\Users\x`: the drive-letter form a `file:` URL carries
/// a Windows path in, after percent-decoding. `None` for anything else.
// Called only by the Windows build; its tests run everywhere.
#[cfg_attr(unix, allow(dead_code))]
fn windows_drive_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let drive_absolute = bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && bytes[3] == b'/';
    drive_absolute.then(|| path[1..].replace('/', "\\"))
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
/// A stray `ESC` interrupts a cwd payload rather than reporting a partial path.
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

#[cfg(test)]
mod tests {
    use super::windows_drive_path;

    #[test]
    fn a_drive_letter_url_path_becomes_a_windows_path() {
        assert_eq!(
            windows_drive_path("/C:/Users/op").as_deref(),
            Some("C:\\Users\\op")
        );
        assert_eq!(windows_drive_path("/home/op"), None);
        assert_eq!(windows_drive_path("/C:"), None);
    }
}
