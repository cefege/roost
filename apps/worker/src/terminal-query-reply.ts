// Synthesize replies to terminal capability queries that @wterm/core leaves
// UNANSWERED. Verified empirically 2026-07-05 against @wterm/core 0.3.0
// (roost-patched wasm): the core answers cursor-DSR (ESC[6n) only and is SILENT
// on Primary Device Attributes (ESC[c / ESC[0c) and XTVERSION (ESC[>0q).
//
// Full-screen TUIs may issue Primary DA and XTVERSION at startup, then choose
// a render path based on the reply. We answer honestly so any terminal client
// can feature-detect this emulator deterministically.
//
// Called from session-manager.ts on every LIVE PTY chunk; the reply is written
// BACK into the PTY (where an app reads it on stdin). DSR is handled separately
// via wtermCore.getResponse() (the core DOES answer that one).

const ESC = 0x1b;
const LBRACKET = 0x5b; // [
const GT = 0x3e;       // >
const ZERO = 0x30;     // 0
const LOWER_C = 0x63;  // c
const LOWER_Q = 0x71;  // q

// Primary DA reply: VT100 + Advanced Video Option (ESC[?1;2c) — the universal
// "I am a terminal" handshake, enough to unblock any DA-gated init. Matches
// what xterm reports as its baseline class.
const PRIMARY_DA_REPLY = "\x1b[?1;2c";
// XTVERSION reply: DCS > | <name> ST. Report the emulator we actually run so a
// client that version-gates behaviour sees a real name instead of silence.
const XTVERSION_REPLY = "\x1bP>|wterm(roost)\x1b\\";

/**
 * "" when there are none. Queries normally arrive as one contiguous write, so
 * per-chunk scanning answers each query exactly once without a carry window.
 * Replies preserve the order in which queries appeared.
 */
export function synthQueryReplies(chunk: Uint8Array): string {
  let out = "";
  const n = chunk.length;
  for (let i = 0; i + 2 < n; i++) {
    if (chunk[i] !== ESC || chunk[i + 1] !== LBRACKET) continue;
    const c2 = chunk[i + 2];
    // ESC [ c  → Primary DA (no param). NOT ESC[>c (secondary): that has '>'
    // at c2, so this branch can't false-match it.
    if (c2 === LOWER_C) { out += PRIMARY_DA_REPLY; continue; }
    // ESC [ 0 c → Primary DA with explicit 0 param.
    if (c2 === ZERO && chunk[i + 3] === LOWER_C) { out += PRIMARY_DA_REPLY; i += 1; continue; }
    // ESC [ > 0 q → XTVERSION.
    if (c2 === GT && chunk[i + 3] === ZERO && chunk[i + 4] === LOWER_Q) { out += XTVERSION_REPLY; i += 2; continue; }
  }
  return out;
}
