// Pure terminal byte-stream scanners — no SessionManager state, no I/O.
// _scanAltModeTransitions tracks alt-screen enter/leave (DEC private modes
// 47/1047/1049) so the worker knows when claude/vim/htop own the screen
// (drives rec.alt_mode → the c03d62d0 rebuild carve-out + scrollback gating).
// _scanOsc7 extracts cwd-change (OSC 7) sequences for cwd tracking.
// Sole caller: session-manager.ts::emitUpstreamChunk / resume. Split out of
// session-manager.ts (400-line cap); behavior byte-for-byte unchanged.

// All alt-screen toggles are `ESC [ ? N { h | l }` where N ∈ {47, 1047, 1049}.
// h=enter, l=leave. 1049 is the modern variant (saves cursor + clears alt
// buffer); 47 is legacy; 1047 is in-between (no cursor save).
export const ALT_ENTER_SEQS: ReadonlyArray<Uint8Array> = [
  new Uint8Array([0x1b, 0x5b, 0x3f, 0x31, 0x30, 0x34, 0x39, 0x68]), // ESC[?1049h
  new Uint8Array([0x1b, 0x5b, 0x3f, 0x34, 0x37, 0x68]),              // ESC[?47h
  new Uint8Array([0x1b, 0x5b, 0x3f, 0x31, 0x30, 0x34, 0x37, 0x68]), // ESC[?1047h
];
const ALT_EXIT_SEQS: ReadonlyArray<Uint8Array> = [
  new Uint8Array([0x1b, 0x5b, 0x3f, 0x31, 0x30, 0x34, 0x39, 0x6c]),
  new Uint8Array([0x1b, 0x5b, 0x3f, 0x34, 0x37, 0x6c]),
  new Uint8Array([0x1b, 0x5b, 0x3f, 0x31, 0x30, 0x34, 0x37, 0x6c]),
];
// Built once at module load — _scanAltModeTransitions runs per PTY chunk (hot
// path), so the enter/exit tag table must not be rebuilt on every call.
const _ALT_SEQS: ReadonlyArray<{ seq: Uint8Array; enter: boolean }> = [
  ...ALT_ENTER_SEQS.map((seq) => ({ seq, enter: true })),
  ...ALT_EXIT_SEQS.map((seq) => ({ seq, enter: false })),
];

// OSC 7 max payload kept across chunk boundaries. The whole sequence
// is `ESC ] 7 ; file:// <host> / <percent-encoded-path> BEL` — paths
// over ~1 KB are absurd and would suggest binary noise, so we cap and
// drop the rest. ESC = 0x1b, BEL = 0x07, ST = ESC \\.
const OSC7_CARRY_MAX = 1024;
const _OSC7_PREFIX = new Uint8Array([0x1b, 0x5d, 0x37, 0x3b]); // ESC ] 7 ;

function _bufIndexOf(haystack: Uint8Array, needle: Uint8Array, fromIndex = 0): number {
  if (needle.length === 0 || needle.length > haystack.length - fromIndex) return -1;
  outer: for (let i = fromIndex; i <= haystack.length - needle.length; i++) {
    for (let j = 0; j < needle.length; j++) {
      if (haystack[i + j] !== needle[j]) continue outer;
    }
    return i;
  }
  return -1;
}

/** Scan `combined` for the LAST occurrence of any alt-screen enter or
 *  exit sequence. Returns the new alt_mode if found; null if no
 *  transition. */
export function _scanAltModeTransitions(combined: Uint8Array, prevAlt: boolean): boolean {
  let altMode = prevAlt;
  let cursor = 0;
  while (cursor < combined.length) {
    let bestIdx = -1;
    let bestEnter = false;
    for (const { seq, enter } of _ALT_SEQS) {
      const idx = _bufIndexOf(combined, seq, cursor);
      if (idx !== -1 && (bestIdx === -1 || idx < bestIdx)) {
        bestIdx = idx;
        bestEnter = enter;
      }
    }
    if (bestIdx === -1) break;
    altMode = bestEnter;
    cursor = bestIdx + 1;
  }
  return altMode;
}

// Scan `combined` for OSC 7 cwd-change sequences. Returns:
//   { newCwd: string | null, carry: Uint8Array }
// where `carry` is the unparsed tail (possibly an in-progress sequence
// that should be re-fed on the next chunk). On match, newCwd is the
// LAST complete OSC 7 path observed in the chunk — multiple cd's in
// one chunk collapse to the final destination.
export function _scanOsc7(combined: Uint8Array): { newCwd: string | null; carry: Uint8Array } {
  let cursor = 0;
  let lastCwd: string | null = null;
  while (cursor < combined.length) {
    const start = _bufIndexOf(combined, _OSC7_PREFIX, cursor);
    if (start === -1) {
      // No more starts. Carry the tail that could be a prefix of a
      // future ESC ] 7 ; — at most _OSC7_PREFIX.length - 1 bytes.
      const carryFrom = Math.max(cursor, combined.length - (_OSC7_PREFIX.length - 1));
      return { newCwd: lastCwd, carry: combined.subarray(carryFrom) };
    }
    // Hunt for terminator: BEL (0x07) or ST (ESC \\). Strict — abort
    // on stray ESC inside the payload (would indicate the sequence got
    // interrupted by something else, treat as garbage).
    let termIdx = -1;
    let termLen = 0;
    for (let i = start + _OSC7_PREFIX.length; i < combined.length; i++) {
      const b = combined[i]!;
      if (b === 0x07) { termIdx = i; termLen = 1; break; }
      if (b === 0x1b) {
        if (i + 1 < combined.length && combined[i + 1] === 0x5c) {
          termIdx = i; termLen = 2; break;
        }
        // ESC not followed by \\ → in-progress sequence or interrupted.
        // Treat as not-yet-terminated and carry the rest.
        break;
      }
    }
    if (termIdx === -1) {
      // Partial sequence — carry from `start` so we re-parse next chunk.
      // Cap the carry so a stream without a terminator can't grow.
      const carryFrom = combined.length - start > OSC7_CARRY_MAX
        ? combined.length - OSC7_CARRY_MAX
        : start;
      return { newCwd: lastCwd, carry: combined.subarray(carryFrom) };
    }
    // Payload is `file://<host>/<percent-encoded-path>`. Strip the
    // `file://` and the host segment up to the next `/`. Decode the
    // payload bytes as UTF-8 — Apple Terminal's chpwd hook percent-
    // encodes everything, but custom PROMPT_COMMAND hooks (zsh-vcs,
    // starship, prezto) often emit raw UTF-8 bytes for non-ASCII
    // directory names. String.fromCharCode would mojibake those into
    // Latin-1 codepoints that decodeURIComponent passes through
    // unchanged → wrong cwd recorded.
    const payload = combined.subarray(start + _OSC7_PREFIX.length, termIdx);
    const raw = new TextDecoder("utf-8", { fatal: false }).decode(payload);
    const m = /^file:\/\/[^/]*(\/.*)$/.exec(raw);
    if (m) {
      try { lastCwd = decodeURIComponent(m[1]!); }
      catch { lastCwd = m[1]!; }
    }
    cursor = termIdx + termLen;
  }
  return { newCwd: lastCwd, carry: new Uint8Array(0) };
}
