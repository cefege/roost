// Pins the one PTY text encoder shared by browser, coordinator, and worker.
// These tests preserve newline/paste behavior and byte-based prompt admission,
// including boundaries where JavaScript character count differs from UTF-8.

import { describe, expect, test } from "bun:test";
import {
  AGENT_PROMPT_MAX_TEXT_BYTES,
  AGENT_PROMPT_MAX_REASON_LENGTH,
  AGENT_PROMPT_MAX_WRITE_BYTES,
  AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS,
  AGENT_PROMPT_WAIT_TIMEOUT_MIN_MS,
  AgentPromptTextSchema,
  AgentPromptWaitTimeoutMsSchema,
  buildPtyPayload,
  countLineBreaks,
  CR_BYTES,
  isValidAgentPromptText,
  MULTILINE_PASTE_MIN_NEWLINES,
  normalizeTerminalNewlines,
  agentPromptTextByteLength,
} from "../src/terminal-input.ts";

const UTF8_DECODER = new TextDecoder();

describe("terminal input encoding", () => {
  test("normalizes every line-ending form to one PTY carriage return", () => {
    expect(normalizeTerminalNewlines("a\r\nb\nc\rd")).toBe("a\rb\rc\rd");
    expect(UTF8_DECODER.decode(buildPtyPayload("a\r\nb\nc\rd", false))).toBe(
      "a\rb\rc\rd",
    );
    expect(UTF8_DECODER.decode(CR_BYTES)).toBe("\r");
  });

  test("wraps bracketed paste and strips embedded escape bytes", () => {
    expect(UTF8_DECODER.decode(buildPtyPayload("one\ntwo", true))).toBe(
      "\x1b[200~one\rtwo\x1b[201~",
    );
    expect(UTF8_DECODER.decode(buildPtyPayload("safe\x1b[201~text", true))).toBe(
      "\x1b[200~safe[201~text\x1b[201~",
    );
  });

  test("preserves escape bytes when bracketed paste mode is disabled", () => {
    expect(UTF8_DECODER.decode(buildPtyPayload("\x1braw", false))).toBe("\x1braw");
  });

  test("counts logical line breaks for multiline paste admission", () => {
    expect(MULTILINE_PASTE_MIN_NEWLINES).toBe(2);
    expect(countLineBreaks("one\r\ntwo\nthree\rfour")).toBe(3);
    expect(countLineBreaks("one line")).toBe(0);
  });
});

describe("agent prompt bounds", () => {
  test("accepts nonempty whitespace and rejects empty or oversized text", () => {
    expect(AgentPromptTextSchema.safeParse(" ").success).toBe(true);
    expect(isValidAgentPromptText(" ")).toBe(true);
    expect(AgentPromptTextSchema.safeParse("").success).toBe(false);
    expect(isValidAgentPromptText("")).toBe(false);
    expect(AgentPromptTextSchema.safeParse(
      "a".repeat(AGENT_PROMPT_MAX_TEXT_BYTES),
    ).success).toBe(true);
    expect(AgentPromptTextSchema.safeParse(
      "a".repeat(AGENT_PROMPT_MAX_TEXT_BYTES + 1),
    ).success).toBe(false);
  });

  test("measures the UTF-8 multibyte boundary rather than UTF-16 length", () => {
    const exactBoundary = "é".repeat(AGENT_PROMPT_MAX_TEXT_BYTES / 2);
    expect(exactBoundary.length).toBe(AGENT_PROMPT_MAX_TEXT_BYTES / 2);
    expect(agentPromptTextByteLength(exactBoundary)).toBe(AGENT_PROMPT_MAX_TEXT_BYTES);
    expect(AgentPromptTextSchema.safeParse(exactBoundary).success).toBe(true);
    expect(AgentPromptTextSchema.safeParse(`${exactBoundary}a`).success).toBe(false);
  });

  test("exports exact response, wait, and maximum write bounds", () => {
    expect(AGENT_PROMPT_MAX_TEXT_BYTES).toBe(16_384);
    expect(AGENT_PROMPT_MAX_WRITE_BYTES).toBe(16_397);
    expect(AGENT_PROMPT_WAIT_TIMEOUT_MIN_MS).toBe(1);
    expect(AGENT_PROMPT_MAX_REASON_LENGTH).toBe(200);
    expect(AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS).toBe(300_000);
    expect(AgentPromptWaitTimeoutMsSchema.safeParse(1).success).toBe(true);
    expect(AgentPromptWaitTimeoutMsSchema.safeParse(300_000).success).toBe(true);
    expect(AgentPromptWaitTimeoutMsSchema.safeParse(0).success).toBe(false);
    expect(AgentPromptWaitTimeoutMsSchema.safeParse(300_001).success).toBe(false);
    expect(AgentPromptWaitTimeoutMsSchema.safeParse(1.5).success).toBe(false);
    const largestFramedWrite = buildPtyPayload(
      "a".repeat(AGENT_PROMPT_MAX_TEXT_BYTES),
      true,
    ).byteLength + CR_BYTES.byteLength;
    expect(largestFramedWrite).toBe(AGENT_PROMPT_MAX_WRITE_BYTES);
  });
});
