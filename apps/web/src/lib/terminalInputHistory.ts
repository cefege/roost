// terminalInputHistory — per-session ring of the user's recently-typed text,
// the highest-signal source for keyterm extraction: you SAY what you TYPE, so a
// command/prompt you entered names the exact tokens you're about to dictate.
//
// CellTerminal feeds raw onData strings (mode-encoded keystrokes); we strip the
// escape sequences + control bytes and keep printable runs, capped per session.
// Read by MobileVoiceInput via keytermContext (TerminalContext.input).
//
// Owner of this state: this module. recordInput / getInputText / clearInput —
// grep these. Mirrors the module-registry pattern in the sync layer.

const MAX_CHARS = 2000;

const history = new Map<string, string>();

// CSI / DCS / lone-ESC sequences + control bytes → dropped; CR/LF/backspace →
// space (word separator). Backspace-as-space can mis-split a word mid-edit;
// keyterm biasing tolerates the occasional fragment.
function printableRun(s: string): string {
  return s
    .replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, "") // CSI ESC[...
    .replace(/\x1b[PX^_].*?(?:\x1b\\|\x07)/g, "") // DCS/PM/APC/SOS strings
    .replace(/\x1b[@-Z\\-_]/g, "") // other 2-byte ESC sequences
    .replace(/[\x00-\x08\x0b-\x1f\x7f]/g, " ") // control bytes → space
    .replace(/[\r\n]+/g, " ");
}

/** Append a raw onData string to the session's typed-text ring. */
export function recordInput(sessionId: string, data: string): void {
  // Fast path for the common case — a single printable ASCII char (ordinary
  // typing) needs none of the 5 escape/control regexes below.
  if (data.length === 1 && data >= " " && data <= "~") {
    const next = (history.get(sessionId) ?? "") + data;
    history.set(sessionId, next.length > MAX_CHARS ? next.slice(next.length - MAX_CHARS) : next);
    return;
  }
  const add = printableRun(data);
  if (!add.trim()) return;
  const next = (history.get(sessionId) ?? "") + add;
  history.set(sessionId, next.length > MAX_CHARS ? next.slice(next.length - MAX_CHARS) : next);
}

/** Recently-typed text for the session (oldest→newest), or "" if none. */
export function getInputText(sessionId: string): string {
  return history.get(sessionId) ?? "";
}

/** Drop a session's ring on pane close. */
export function clearInput(sessionId: string): void {
  history.delete(sessionId);
}

/** Drop typed terminal context retained for dictation across all sessions. */
export function clearInputHistoryForLogout(): void {
  history.clear();
}
