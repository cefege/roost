// Shared PTY paste encoding. Callers choose the current terminal paste mode;
// submitters append CR separately when they need to execute the text.

const BP_START = "\x1b[200~";
const BP_END = "\x1b[201~";
const ESC = "\x1b";

export function buildPtyPayload(text: string, bracketedPaste: boolean): Uint8Array {
  const payload = bracketedPaste
    ? `${BP_START}${text.replaceAll(ESC, "")}${BP_END}`
    : text;
  return new TextEncoder().encode(payload);
}

// CR is a separate frame so the terminal text gateway can append it only for
// submissions, after the enterDelayMs window.

export const CR_BYTES = new TextEncoder().encode("\r");

// The receiver needs time to ingest a big message before the Enter lands, or
// it gets eaten. Scale with length: 150ms floor + ~0.4ms/char, capped at 2.5s.
export function enterDelayMs(text: string): number {
  return Math.min(150 + Math.ceil(text.length * 0.4), 2500);
}
