// Pure terminal byte-stream scanners — no SessionManager state, no I/O.
// _scanAltModeTransitions tracks alt-screen enter/leave (DEC private modes
// 47/1047/1049) so the worker knows when claude/vim/htop own the screen
// (drives rec.alt_mode → the c03d62d0 rebuild carve-out + scrollback gating).
// _scanOsc7 extracts cwd-change (OSC 7) sequences for cwd tracking.
// _scanAgentOsc extracts title/progress metadata for agent-state fallback.
// Sole caller: session-scrollback.ts. Split out of session-manager.ts.
import { supportedHostPlatform, type SupportedHostPlatform } from "@roost/shared/platform";
import { parseOsc7WorkerPath } from "./util/path.ts";

const HOST_PLATFORM = supportedHostPlatform();


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
export function _scanOsc7(
  combined: Uint8Array,
  platform: SupportedHostPlatform = HOST_PLATFORM,
): { newCwd: string | null; carry: Uint8Array } {
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
    // Payload is `file://<host>/<percent-encoded-path>`. The worker path
    // parser preserves historical POSIX behavior and reconstructs canonical
    // Windows `C:/...` and `//server/share/...` paths from drive/UNC OSC 7.
    // Decode payload bytes as UTF-8 first: custom prompts often emit raw UTF-8
    // rather than percent-encoding non-ASCII directory names.
    const payload = combined.subarray(start + _OSC7_PREFIX.length, termIdx);
    const raw = new TextDecoder("utf-8", { fatal: false }).decode(payload);
    const parsedPath = parseOsc7WorkerPath(raw, platform);
    if (parsedPath !== null) lastCwd = parsedPath;
    cursor = termIdx + termLen;
  }
  return { newCwd: lastCwd, carry: new Uint8Array(0) };
}

const AGENT_OSC_CARRY_MAX = 1024;
// eslint-disable-next-line no-control-regex
const OSC_CONTROL_RE = /[\x00-\x1f\x7f]/g;

// OSC 0/2 title and OSC 9 progress bodies are user-visible text (surfaced
// verbatim in sessionTitle.ts), so a plain `.slice(0, N)` on this UTF-16
// string can cut a surrogate pair in half and leave a lone surrogate that
// renders as U+FFFD downstream. Codepoint iteration (not full grapheme
// segmentation — this is a raw capture buffer, not a render path; the web
// side re-truncates with cluster awareness for display) is enough to avoid
// that, and only ever appends whole codepoints, so the result never exceeds
// `max` UTF-16 code units, the unit the cap protects.
function _truncateCodepoints(str: string, max: number): string {
  if (str.length <= max) return str;
  let result = "";
  for (const ch of str) {
    if (result.length + ch.length > max) break;
    result += ch;
  }
  return result;
}

export interface AgentOscScan {
  title: string | null;
  progress: string | null;
  carry: string;
}

/** Per-session OSC-scan state carried on the session record. */
export interface AgentOscState {
  agentOscDecoder: TextDecoder;
  agentOscCarry: string;
  rawOscTitle: string;
  rawOscProgress: string;
}

/** Fresh scan state for one session. The streaming decoder preserves split
 *  UTF-8 across PTY chunks; the carry preserves a split OSC sequence. */
export function initAgentOscState(): AgentOscState {
  return {
    agentOscDecoder: new TextDecoder("utf-8", { fatal: false }),
    agentOscCarry: "",
    rawOscTitle: "",
    rawOscProgress: "",
  };
}

/** Scan complete OSC 0/2 title and OSC 9 progress sequences. `combined`
 * includes the caller's prior carry; the returned bounded tail must be
 * prepended to the next decoded chunk. */
export function _scanAgentOsc(combined: string): AgentOscScan {
  let title: string | null = null;
  let progress: string | null = null;
  let cursor = 0;
  while (cursor < combined.length) {
    const start = combined.indexOf("\x1b]", cursor);
    if (start < 0) break;
    let end = combined.indexOf("\x07", start + 2);
    let termLength = 1;
    const st = combined.indexOf("\x1b\\", start + 2);
    if (st >= 0 && (end < 0 || st < end)) {
      end = st;
      termLength = 2;
    }
    if (end < 0) {
      return {
        title,
        progress,
        carry: combined.slice(Math.max(start, combined.length - AGENT_OSC_CARRY_MAX)),
      };
    }
    const payload = combined.slice(start + 2, end);
    const separator = payload.indexOf(";");
    const code = separator < 0 ? "" : payload.slice(0, separator);
    const body = separator < 0 ? "" : payload.slice(separator + 1);
    if (code === "0" || code === "2") {
      title = _truncateCodepoints(body.replace(OSC_CONTROL_RE, ""), 256);
    } else if (code === "9" && body.startsWith("4;")) {
      progress = _truncateCodepoints(body.replace(OSC_CONTROL_RE, ""), 64);
    }
    cursor = end + termLength;
  }
  const trailingEsc = combined.endsWith("\x1b") ? "\x1b" : "";
  return { title, progress, carry: trailingEsc };
}
