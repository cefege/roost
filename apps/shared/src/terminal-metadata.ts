// Incremental terminal metadata parsing shared by workers and coordinators.
// It owns the OSC 0/2 title grammar and normalization contract used on both
// sides of the worker transport, so rolling upgrades cannot drift in title
// semantics while raw compatibility remains available.

export const TERMINAL_METADATA_CAPABILITY = "terminal_metadata_v1";
export const TERMINAL_TITLE_CARRY_CAP = 1_024;
export const TERMINAL_TITLE_MAX_LENGTH = 256;
export const TERMINAL_METADATA_ACTIVITY_THROTTLE_MS = 60_000;

// eslint-disable-next-line no-control-regex
const TITLE_CONTROL_RE = /[\x00-\x1f\x7f]/g;
// eslint-disable-next-line no-control-regex
const TITLE_SPINNER_RE = /[\u2800-\u28FF]/g;
// ESC inside a title body aborts that candidate. OSC 0/2 can end with BEL or ST.
// eslint-disable-next-line no-control-regex
const OSC_TITLE_RE = /\x1b\][02];([^\x07\x1b]*)(?:\x07|\x1b\\)/g;

export interface TerminalTitleObservation {
  title: string;
  dedupKey: string;
}

/** Normalize an already decoded title before it reaches retained coordinator state. */
export function normalizeTerminalTitle(raw: string): TerminalTitleObservation {
  const title = raw.replace(TITLE_CONTROL_RE, "").slice(0, TERMINAL_TITLE_MAX_LENGTH);
  return { title, dedupKey: title.replace(TITLE_SPINNER_RE, "\u2800") };
}

/** Stateful OSC 0/2 parser for one ordered PTY byte stream. */
export class TerminalTitleParser {
  #decoder = new TextDecoder("utf-8", { fatal: false });
  #carry = "";

  /** Returns the latest complete title in this chunk, if any. */
  push(bytes: Uint8Array): TerminalTitleObservation | null {
    if (bytes.byteLength === 0) return null;
    const combined = this.#carry + this.#decoder.decode(bytes, { stream: true });
    if (!combined.includes("\x1b]")) {
      this.#carry = combined.endsWith("\x1b") ? "\x1b" : "";
      return null;
    }

    let latest: string | null = null;
    let lastEnd = 0;
    OSC_TITLE_RE.lastIndex = 0;
    let match: RegExpExecArray | null;
    while ((match = OSC_TITLE_RE.exec(combined)) !== null) {
      latest = match[1]!;
      lastEnd = OSC_TITLE_RE.lastIndex;
    }
    this.#carry = boundedCarry(combined.slice(lastEnd));
    return latest === null ? null : normalizeTerminalTitle(latest);
  }
}

function boundedCarry(value: string): string {
  return value.length <= TERMINAL_TITLE_CARRY_CAP
    ? value
    : value.slice(-TERMINAL_TITLE_CARRY_CAP);
}
