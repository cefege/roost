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
