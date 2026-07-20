// Shared PTY paste encoding — used by BOTH mic dictation (MobileVoiceInput)
// and the terminal context-menu Paste (TerminalContextMenu). One system so
// paste and dictation insert text identically.
//
// Bracket ONLY multi-line text (ESC[200~…ESC[201~) so an embedded newline
// doesn't submit early; single-line goes raw. Callers append CR themselves
// if they want to submit (mic does; paste does not).

const BP_START = "\x1b[200~";
const BP_END = "\x1b[201~";

export function buildPtyPayload(text: string): Uint8Array {
  const body = text.includes("\n") ? `${BP_START}${text}${BP_END}` : text;
  return new TextEncoder().encode(body);
}

// CR as its own frame — callers that want to SUBMIT append this after the
// enterDelayMs window. Shared by mic dictation and the keyboard composer.
export const CR_BYTES = new TextEncoder().encode("\r");

// The receiver needs time to ingest a big message before the Enter lands, or
// it gets eaten. Scale with length: 150ms floor + ~0.4ms/char, capped at 2.5s.
export function enterDelayMs(text: string): number {
  return Math.min(150 + Math.ceil(text.length * 0.4), 2500);
}
