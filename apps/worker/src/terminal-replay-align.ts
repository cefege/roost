// Parser alignment for a replay into a FRESH terminal core.
//
// A rebuild feeds the retained raw PTY window into a brand-new @wterm/core, and
// that core's VT parser starts COLD: no half-consumed sequence, no pending CSI
// parameters, nothing. So the FIRST byte handed to it MUST be the start of a
// token. Hand it `32m` and it does not resume an `ESC [` it never saw — it
// prints `32m` as literal text into row 0, and on an alt-screen session that
// text is permanent: the TUI's differential redraw only rewrites cells it
// believes are stale, and it believes it already painted that one.
//
// Neither cut point the rebuild splices at is sequence-aware, and neither can be
// made so:
//   - the resize boundary is `rec.head_seq` sampled at the keeper's result
//     frame, and head_seq advances by WHOLE PTY CHUNKS, so the boundary lands
//     wherever the pty happened to flush — including between `ESC [` and `32m`.
//   - the oldest retained ring byte is wherever eviction last overwrote, which
//     is an arbitrary byte offset by construction.
// Both are repaired here, in OPPOSITE directions: a boundary has the bytes that
// opened the split sequence still behind it, so it is REWOUND onto them; an
// eviction cut has nothing behind it at all, so it is ADVANCED past the orphan.
//
// Pure, bounded, and allocation-free: the rebuild is the most state-heavy thing
// the worker does, and this is the one part of it that is a decidable function
// of (bytes, offset) with no session, no core, and no I/O in reach.

/** The only byte a cold parser can be handed as a sequence start. */
const ESC = 0x1b;
const CSI_OPEN = 0x5b; // [
/** A CSI ends at the first byte in 0x40-0x7e; everything below is parameters or
 *  intermediates. Same range terminal-query-reply.ts tokenizes probes with. */
const CSI_FINAL_MIN = 0x40;
const CSI_FINAL_MAX = 0x7e;
/** Intermediates of an nF escape (`ESC ( B`). The final is the first byte at or
 *  above 0x30, so a classic 2-byte `ESC M` is just the zero-intermediate case of
 *  the same walk and needs no separate branch. */
const INTERMEDIATE_MIN = 0x20;
const INTERMEDIATE_MAX = 0x2f;
const BEL = 0x07;
/** String-sequence introducers — DCS, SOS, OSC, PM, APC. They are closed by BEL
 *  or ST, never by a CSI final, and their payloads legitimately contain bytes in
 *  the CSI final range (an OSC 7 cwd is full of `/`). Checked before the nF walk
 *  because `]` (0x5d) is itself inside 0x40-0x7e. */
const DCS_OPEN = 0x50; // P
const SOS_OPEN = 0x58; // X
const OSC_OPEN = 0x5d; // ]
const PM_OPEN = 0x5e; // ^
const APC_OPEN = 0x5f; // _

/** Bounded look-behind. Long enough for every sequence the rebuild path must not
 *  split (the deepest are DECRQM/DECRPM and an OSC 8 hyperlink's target), short
 *  enough that the scan is a fixed cost on a path already doing a full WASM core
 *  construction. Past the cap the answer is `start`: an unterminated sequence
 *  256 bytes long is a stream that is already lying, and eating one remnant of
 *  literal text beats walking a 1 MiB ring backwards on every rebuild. */
export const MAX_SEQ_LOOKBEHIND = 256;

/** Does the sequence introduced by the ESC at `k` close inside [k, end)?
 *
 *  Only ever called from the backward scan below, so `k` is the NEAREST ESC
 *  under `end` and the span (k, end) is ESC-FREE. That is why ST (`ESC \`) is
 *  not tested for here: an ST inside the window would have been found as its own
 *  ESC first, and rewinding to THAT ESC is the correct answer for a boundary
 *  that splits it. */
function terminatedBefore(bytes: Uint8Array, k: number, end: number): boolean {
  // A trailing bare ESC is unterminated by definition — even the shortest escape
  // needs one byte more than the window holds.
  if (k + 1 >= end) return false;
  const introducer = bytes[k + 1]!;
  if (introducer === CSI_OPEN) {
    for (let i = k + 2; i < end; i++) {
      const b = bytes[i]!;
      if (b >= CSI_FINAL_MIN && b <= CSI_FINAL_MAX) return true;
    }
    return false;
  }
  if (
    introducer === OSC_OPEN || introducer === DCS_OPEN || introducer === APC_OPEN ||
    introducer === PM_OPEN || introducer === SOS_OPEN
  ) {
    for (let i = k + 2; i < end; i++) if (bytes[i] === BEL) return true;
    return false;
  }
  // nF or 2-byte escape: skip intermediates, then the final must be present.
  // `ESC M` terminates at k+1 with zero intermediates; `ESC ( B` cut after
  // `ESC (` does not terminate, and rewinding is exactly what stops the `B`
  // from printing as the letter B.
  let i = k + 1;
  while (i < end && bytes[i]! >= INTERMEDIATE_MIN && bytes[i]! <= INTERMEDIATE_MAX) i++;
  return i < end;
}

/** Rewind `start` to the beginning of an escape sequence that `start` splits.
 *  Returns an index <= start; returns `start` unchanged when `start` is already
 *  at a token boundary or no unterminated sequence lead-in is found within the
 *  bounded look-behind.
 *
 *  The result moves the PARSER's resume point, not the content boundary: the
 *  bytes in [aligned, start) were already scanned — and any probe among them
 *  already answered — by the live path, so a caller replays them to prime the
 *  parser and MUST NOT re-interpret them. */
export function rewindToSequenceStart(bytes: Uint8Array, start: number): number {
  const from = start < bytes.length ? start : bytes.length;
  if (from <= 0) return start;
  const floor = from > MAX_SEQ_LOOKBEHIND ? from - MAX_SEQ_LOOKBEHIND : 0;
  // Backward, so the first ESC found is the NEAREST one. That single fact
  // carries the whole correctness argument: an ESC always restarts the parser,
  // so nothing before the nearest ESC can still be open at `start`, and the only
  // two possibilities left are "this sequence closed" (start is already at a
  // token boundary — return it untouched, never over-rewind across completed
  // work) and "this sequence is still open" (start splits it — return its head).
  for (let k = from - 1; k >= floor; k--) {
    if (bytes[k] !== ESC) continue;
    return terminatedBefore(bytes, k, from) ? start : k;
  }
  return start;
}

/** Bytes at the head of an evicted ring have no parser context: a leading
 *  sequence REMNANT would print as literal text. Returns the offset of the
 *  first byte safe to replay.
 *
 *  Forward, not backward, because an eviction cut has NOTHING behind it to
 *  rewind onto — the bytes that opened the split sequence were overwritten in
 *  place by the ring. Two distinct kinds of damage sit at that cut, and only
 *  these two, because everything after them is self-describing:
 *
 *    1. A split multi-byte character. Far more common than a split sequence —
 *       a UTF-8 continuation byte (0x80-0xbf) can never START a codepoint, so
 *       a cut inside one renders U+FFFD. At most 3 of them can lead.
 *    2. A split escape sequence, whose remnant would print as text.
 *
 *  The ESC scan is BOUNDED by the same argument the rewind side makes: an
 *  orphan remnant is at most one sequence long, so an ESC further out than
 *  MAX_SEQ_LOOKBEHIND proves the cut landed in ordinary TEXT — which needs no
 *  skipping at all. Returning the unbounded `indexOf` there instead would
 *  discard every byte up to the first ESC in the window (all of it, for a
 *  window with no ESC at all: a build log, a `cat` of a big file), turning a
 *  256-byte repair into a total loss of retained history. Worst case is now
 *  bounded at a partial codepoint plus one sequence remnant. */
export function skipOrphanSequencePrefix(bytes: Uint8Array): number {
  // A leading continuation byte belongs to a codepoint whose lead byte is gone.
  let start = 0;
  while (start < bytes.length && (bytes[start]! & 0xc0) === 0x80) start++;
  const limit = Math.min(bytes.length, start + MAX_SEQ_LOOKBEHIND);
  for (let i = start; i < limit; i++) {
    if (bytes[i] === ESC) return i;
  }
  // No sequence within reach: the cut is in text, so replay everything that
  // still forms whole codepoints.
  return start;
}
