//! Parser alignment for replaying an evicted raw PTY window into a cold core.
//! The survivor-adoption replay (`session::resume_core`) and a core rebuild
//! from the ring call [`skip_orphan_sequence_prefix`] on the FIRST bytes a cold
//! core is handed, and only when the window was evicted. Ports
//! `apps/worker/src/terminal/terminal-replay-align.ts`.
//!
//! The oldest retained byte is wherever eviction last overwrote — an arbitrary
//! offset. A leading UTF-8 continuation byte or escape-sequence remnant has no
//! parser context and would render as replacement or literal text
//! (production: `32m1969M` burned into an htop grid), and nothing downstream
//! ever re-parses it.

/// The only byte a cold parser can be handed as a sequence start.
const ESC: u8 = 0x1b;

/// Bounded scan for an escape-sequence remnant: long enough for the deepest
/// sequence the replay path handles, short enough that a later ESC cannot
/// discard an arbitrary text window.
pub const MAX_SEQ_LOOKBEHIND: usize = 256;

/// The offset of the first byte of an evicted window that is safe to replay.
///
/// Forward, not backward: an eviction cut has nothing behind it to rewind
/// onto. Two kinds of damage sit at the cut, and only these two, because
/// everything after them is self-describing:
///   1. a split multi-byte character — a continuation byte (0x80-0xbf) can
///      never start a codepoint, and at most 3 of them can lead;
///   2. a split escape sequence, whose remnant would print as text.
///
/// The ESC scan is BOUNDED because an orphan remnant is at most one sequence
/// long. An ESC further out than [`MAX_SEQ_LOOKBEHIND`] proves the cut landed
/// in ordinary text, which needs no skipping: scanning unbounded would discard
/// everything up to the first ESC — all of it for a window with none, a build
/// log or a `cat` — turning a small repair into a total loss of history.
pub fn skip_orphan_sequence_prefix(bytes: &[u8]) -> usize {
    let start = bytes
        .iter()
        .position(|&byte| byte & 0xc0 != 0x80)
        .unwrap_or(bytes.len());
    let limit = bytes.len().min(start + MAX_SEQ_LOOKBEHIND);
    bytes[start..limit]
        .iter()
        .position(|&byte| byte == ESC)
        .map_or(start, |found| start + found)
}
