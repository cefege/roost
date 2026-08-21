// Shared PTY paste encoding. Callers choose the current terminal paste mode;
// submitters append CR separately when they need to execute the text.

const BP_START = "\x1b[200~";
const BP_END = "\x1b[201~";
const ESC = "\x1b";

/** PTYs consume Enter as CR. Normalize every clipboard/editor line ending in
 * one pass so Windows CRLF never becomes two terminal Enters. */
export function normalizeTerminalNewlines(text: string): string {
  return text.replace(/\r\n|\r|\n/g, "\r");
}

export function buildPtyPayload(text: string, bracketedPaste: boolean): Uint8Array {
  const normalized = normalizeTerminalNewlines(text);
  const payload = bracketedPaste
    ? `${BP_START}${normalized.replaceAll(ESC, "")}${BP_END}`
    : normalized;
  return new TextEncoder().encode(payload);
}

// A paste of ≥2 newlines into a shell WITHOUT bracketed paste executes every
// line as it arrives — the classic "pasted a script into bash and it ran half
// of it" foot-gun. With bracketed paste on, the shell buffers it safely, so
// there is nothing to warn about and no prompt.
export const MULTILINE_PASTE_MIN_NEWLINES = 2;

export function countLineBreaks(text: string): number {
  return text.match(/\r\n|\r|\n/g)?.length ?? 0;
}

export const CR_BYTES = new TextEncoder().encode("\r");
