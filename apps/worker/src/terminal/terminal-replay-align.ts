// Parser alignment for replaying an evicted raw PTY window into a fresh
// terminal core.
//
// The oldest retained ring byte is wherever eviction last overwrote, which is
// an arbitrary byte offset. A leading UTF-8 continuation byte or escape
// sequence remnant has no parser context and would render as replacement or
// literal text. Repair that cut with a bounded, allocation-free scan.

/** The only byte a cold parser can be handed as a sequence start. */
const ESC = 0x1b;

/** Bounded scan for an escape-sequence remnant. Long enough for the deepest
 * sequence the replay path handles, but short enough to avoid discarding an
 * arbitrary text window merely because a later escape exists. */
const MAX_SEQ_LOOKBEHIND = 256;

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
 *  The ESC scan is BOUNDED because an orphan remnant is at most one sequence
 *  long. An ESC further out than MAX_SEQ_LOOKBEHIND proves the cut landed in
 *  ordinary TEXT — which needs no skipping at all. Returning the unbounded
 *  `indexOf` there instead would discard every byte up to the first ESC in the
 *  window (all of it, for a window with no ESC at all: a build log, a `cat` of
 *  a big file), turning a 256-byte repair into a total loss of retained
 *  history. Worst case is bounded at a partial codepoint plus one sequence
 *  remnant. */
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
