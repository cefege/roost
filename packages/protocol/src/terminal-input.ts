// Owns terminal text normalization, PTY paste framing, and prompt bounds.
// Browser and worker callers choose the terminal's current bracketed-paste mode;
// prompt admission uses the same UTF-8 encoder before any downstream write.

import { z } from "zod";

const BRACKETED_PASTE_START = "\x1b[200~";
const BRACKETED_PASTE_END = "\x1b[201~";
const ESCAPE = "\x1b";
const UTF8_ENCODER = new TextEncoder();

export const CR_BYTES = UTF8_ENCODER.encode("\r");
// Two or more unframed line breaks can execute a partial script as it arrives.
export const MULTILINE_PASTE_MIN_NEWLINES = 2;
export const AGENT_PROMPT_MAX_TEXT_BYTES = 16_384;
// Framing and the submitted CR can add 13 bytes; normalization cannot grow.
export const AGENT_PROMPT_MAX_WRITE_BYTES = AGENT_PROMPT_MAX_TEXT_BYTES
  + BRACKETED_PASTE_START.length
  + BRACKETED_PASTE_END.length
  + CR_BYTES.byteLength;
export const AGENT_PROMPT_MAX_REASON_LENGTH = 200;
export const AGENT_PROMPT_WAIT_TIMEOUT_MIN_MS = 1;
export const AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS = 300_000;

/** PTYs consume Enter as CR. This keeps CRLF from becoming two Enters. */
export function normalizeTerminalNewlines(text: string): string {
  return text.replace(/\r\n|\r|\n/g, "\r");
}

export function buildPtyPayload(text: string, bracketedPaste: boolean): Uint8Array {
  const normalized = normalizeTerminalNewlines(text);
  const payload = bracketedPaste
    ? `${BRACKETED_PASTE_START}${normalized.replaceAll(ESCAPE, "")}${BRACKETED_PASTE_END}`
    : normalized;
  return UTF8_ENCODER.encode(payload);
}

export function countLineBreaks(text: string): number {
  return text.match(/\r\n|\r|\n/g)?.length ?? 0;
}

export function agentPromptTextByteLength(text: string): number {
  return UTF8_ENCODER.encode(text).byteLength;
}

export const AgentPromptTextSchema = z.string()
  .min(1, "prompt text must not be empty")
  .refine(
    (text) => text.length <= AGENT_PROMPT_MAX_TEXT_BYTES
      && agentPromptTextByteLength(text) <= AGENT_PROMPT_MAX_TEXT_BYTES,
    `prompt text must not exceed ${AGENT_PROMPT_MAX_TEXT_BYTES} UTF-8 bytes`,
  );

export const AgentPromptWaitTimeoutMsSchema = z.number()
  .int()
  .min(AGENT_PROMPT_WAIT_TIMEOUT_MIN_MS)
  .max(AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS);

export function isValidAgentPromptText(text: string): boolean {
  return AgentPromptTextSchema.safeParse(text).success;
}
