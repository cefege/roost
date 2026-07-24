// Parser correctness proof for parseOmpLine + fullBlockText.
//
// Inputs are REAL omp transcript JSONL lines captured from ~/.omp/agent/sessions/
// (session version 3). The parser delegates JSONL decoding to the pi SDK's pure
// `parseSessionEntries`; these tests pin the Roost entry→ChatMessage adapter and
// the SDK contract end to end. A live-corpus smoke test proves the whole path on
// a real transcript (skipped when the corpus dir is absent, e.g. CI).

import { test, expect } from "bun:test";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { ChatMessage, ContentBlock } from "@roost/shared/chat/wire";
import { parseOmpLine, fullBlockText, TRUNC_CAP } from "../../../src/chat/omp/parse.ts";

/** Narrow blocks[i] to a concrete kind (checked — throws if absent/mismatched). */
function block<K extends ContentBlock["kind"]>(m: ChatMessage, i: number, kind: K): Extract<ContentBlock, { kind: K }> {
  const b = m.blocks[i];
  if (b === undefined) throw new Error(`no block at index ${i}`);
  if (b.kind !== kind) throw new Error(`block ${i}: expected ${kind}, got ${b.kind}`);
  return b as Extract<ContentBlock, { kind: K }>;
}

// ── Real captured lines (verbatim from the live corpus) ──────────────────────
const REAL = {
  userText: `{"type":"message","id":"46b81954","parentId":"3c8bcbba","timestamp":"2026-07-23T20:21:32.999Z","message":{"role":"user","content":[{"type":"text","text":"it is 57 real surface (not total surface)"}],"attribution":"user","timestamp":1784838088966}}`,
  asstThinkTool: `{"type":"message","id":"f530e127","parentId":"5dfb4ff6","timestamp":"2026-07-23T19:49:58.723Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Let me start by reading the plan file as instructed.","thinkingSignature":""},{"type":"toolCall","id":"call_0167ad99a3704a1d883a8d9e","name":"read","arguments":{"path":"local://price-refresh-july2026-plan.md","i":"Reading authoritative plan"}}],"api":"anthropic-messages","provider":"zai-coding-plan","model":"glm-5.2","stopReason":"toolUse"}}`,
  toolResultErr: `{"type":"message","id":"aaad2543","parentId":"b5e7ba47","timestamp":"2026-07-23T19:53:05.902Z","message":{"role":"toolResult","toolCallId":"call_d6dc057e3bbf42cbac09cfd3","toolName":"grep","content":[{"type":"text","text":"artifact:// ID must be numeric, got: 2;artifact:"}],"details":{},"isError":true,"timestamp":1784836385900}}`,
  developer: `{"type":"message","id":"5dfb4ff6","parentId":"00540d3c","timestamp":"2026-07-23T19:49:52.459Z","message":{"role":"developer","content":[{"type":"text","text":"Plan approved."}],"attribution":"agent","timestamp":1784836192455}}`,
  toolStart: `{"type":"custom","customType":"tool_execution_start","data":{"toolCallId":"call_0167ad99a3704a1d883a8d9e","toolName":"read","startedAt":"2026-07-23T19:49:58.730Z","args":{"path":"x"},"intent":"Reading authoritative plan"},"id":"aa94879b","parentId":"f530e127","timestamp":"2026-07-23T19:49:58.731Z"}`,
  compaction: `{"type":"compaction","id":"7adf7ffb","parentId":"38232656","timestamp":"2026-07-14T08:51:38.583Z","summary":"[Superseded compaction summary elided after a newer compaction]","shortSummary":"Superseded compaction elided","firstKeptEntryId":"fac74ca8","tokensBefore":181416,"fromExtension":false}`,
  image: `{"type":"message","id":"f37a0192","parentId":"ffbe5451","timestamp":"2026-07-13T10:22:02.634Z","message":{"role":"toolResult","toolCallId":"call_00_pBPN9SRYUhk0VFi3FWyZ5328","toolName":"browser","content":[{"type":"text","text":"Screenshot captured"},{"type":"image","data":"blob:sha256:edc40f82d28389d5a5873f030f59b4e2edc9b5057f2c2465016afea334ef5cb3","mimeType":"image/webp"},{"type":"text","text":"done"}],"isError":false,"timestamp":1783938122633}}`,
  // custom_message: extension-injected context, gated on `display`.
  cmVisible: `{"type":"custom_message","customType":"advisor","content":"heads up: verify the scrape","display":true,"id":"03bdddf2","parentId":"c0","timestamp":"2026-07-23T20:00:00.000Z"}`,
  cmHidden: `{"type":"custom_message","customType":"irc:incoming","content":"internal note","display":false,"id":"cm2","parentId":"c1","timestamp":"2026-07-23T20:00:01.000Z"}`,
  // branch_summary: not present in the captured corpus; exact SDK shape.
  branch: `{"type":"branch_summary","id":"b1","parentId":"a0","timestamp":"2026-07-23T20:00:02.000Z","fromId":"x9","summary":"implemented the feature"}`,
};

test("session header line → null (not conversational)", () => {
  expect(parseOmpLine(`{"type":"session","id":"s1","version":3,"timestamp":"2026-01-01T00:00:00Z"}`)).toBeNull();
});

test("fixed-width title slot line → null", () => {
  expect(parseOmpLine(`{"type":"title","v":1,"title":"x","source":"auto","updatedAt":"2026-01-01T00:00:00Z","pad":"   "}`)).toBeNull();
});

test("metadata lines (model_change / mode_change / thinking_level_change) → null", () => {
  expect(parseOmpLine(`{"type":"model_change","id":"m1","parentId":"p","timestamp":"2026-01-01T00:00:00Z","model":"x"}`)).toBeNull();
  expect(parseOmpLine(`{"type":"mode_change","id":"m2","parentId":"p","timestamp":"2026-01-01T00:00:00Z"}`)).toBeNull();
  expect(parseOmpLine(`{"type":"thinking_level_change","id":"m3","parentId":"p","timestamp":"2026-01-01T00:00:00Z"}`)).toBeNull();
});

test("ill-formed JSON → null (never throws)", () => {
  expect(parseOmpLine("{not json")).toBeNull();
  expect(parseOmpLine("")).toBeNull();
});

test("user message → text block, role user, id/parentId/ts preserved", () => {
  const m = parseOmpLine(REAL.userText);
  expect(m).not.toBeNull();
  expect(m!.role).toBe("user");
  expect(m!.id).toBe("46b81954");
  expect(m!.parentId).toBe("3c8bcbba");
  expect(m!.ts).toBe("2026-07-23T20:21:32.999Z");
  expect(m!.blocks).toEqual([{ kind: "text", text: "it is 57 real surface (not total surface)" }]);
});

test("assistant message → thinking + toolCall blocks (order preserved)", () => {
  const m = parseOmpLine(REAL.asstThinkTool);
  expect(m!.role).toBe("assistant");
  expect(m!.blocks.map((b) => b.kind)).toEqual(["thinking", "toolCall"]);
  expect(block(m!, 0, "thinking").text).toBe("Let me start by reading the plan file as instructed.");
  const call = block(m!, 1, "toolCall");
  expect(call.callId).toBe("call_0167ad99a3704a1d883a8d9e");
  expect(call.name).toBe("read");
  expect(JSON.parse(call.argsJson)).toEqual({ path: "local://price-refresh-july2026-plan.md", i: "Reading authoritative plan" });
});

test("toolResult error → toolResult block with message-level metadata + isError", () => {
  const m = parseOmpLine(REAL.toolResultErr);
  expect(m!.role).toBe("toolResult");
  expect(m!.blocks).toEqual([
    { kind: "toolResult", callId: "call_d6dc057e3bbf42cbac09cfd3", name: "grep", isError: true, text: "artifact:// ID must be numeric, got: 2;artifact:", truncated: false, fullLen: 48 },
  ]);
});

test("developer message → role developer (rendered muted, not dropped)", () => {
  const m = parseOmpLine(REAL.developer);
  expect(m!.role).toBe("developer");
  expect(m!.blocks).toEqual([{ kind: "text", text: "Plan approved." }]);
});

test("custom tool_execution_start → toolEvent block (role assistant, phase start)", () => {
  const m = parseOmpLine(REAL.toolStart);
  expect(m!.role).toBe("assistant");
  expect(m!.blocks).toEqual([
    { kind: "toolEvent", callId: "call_0167ad99a3704a1d883a8d9e", name: "read", phase: "start", intent: "Reading authoritative plan" },
  ]);
});

test("top-level compaction entry → muted 'compacted' divider (the drift bug this fixes)", () => {
  const m = parseOmpLine(REAL.compaction);
  expect(m!.role).toBe("developer");
  expect(m!.blocks).toHaveLength(1);
  expect(block(m!, 0, "text").text).toContain("compacted");
});

test("custom_message display:true → developer text block", () => {
  const m = parseOmpLine(REAL.cmVisible);
  expect(m!.role).toBe("developer");
  expect(m!.blocks).toEqual([{ kind: "text", text: "heads up: verify the scrape" }]);
});

test("custom_message display:false → null (omp marks it hidden)", () => {
  expect(parseOmpLine(REAL.cmHidden)).toBeNull();
});

test("branch_summary → developer 'returned from branch' divider", () => {
  const m = parseOmpLine(REAL.branch);
  expect(m!.role).toBe("developer");
  expect(block(m!, 0, "text").text).toBe("— returned from branch: implemented the feature");
});

test("image block blob:sha256 → resolved absolute blob path, interleaved with text", () => {
  const m = parseOmpLine(REAL.image);
  expect(m!.blocks.map((b) => b.kind)).toEqual(["toolResult", "image", "toolResult"]);
  const img = block(m!, 1, "image");
  expect(img.blobPath).toBe(`${process.env.HOME}/.omp/agent/blobs/edc40f82d28389d5a5873f030f59b4e2edc9b5057f2c2465016afea334ef5cb3`);
  expect(img.mime).toBe("image/webp");
});

test("unknown custom type → null (never throws)", () => {
  expect(parseOmpLine(`{"type":"custom","customType":"session_exit","data":{},"id":"c","parentId":"p","timestamp":"2026-01-01T00:00:00Z"}`)).toBeNull();
});

test("unknown message role → null", () => {
  expect(parseOmpLine(`{"type":"message","id":"u","parentId":"p","timestamp":"2026-01-01T00:00:00Z","message":{"role":"bashExecution","content":[{"type":"text","text":"x"}]}}`)).toBeNull();
});

test("empty message (no content blocks) → null", () => {
  expect(parseOmpLine(`{"type":"message","id":"e","parentId":"p","timestamp":"2026-01-01T00:00:00Z","message":{"role":"assistant","content":[]}}`)).toBeNull();
});

test("toolCall id/name aliases (toolCallId/toolName) tolerated", () => {
  const line = `{"type":"message","id":"a","parentId":"p","timestamp":"2026-01-01T00:00:00Z","message":{"role":"assistant","content":[{"type":"toolCall","toolCallId":"c9","toolName":"bash","arguments":{"cmd":"ls"}}]}}`;
  const m = parseOmpLine(line);
  expect(m!.blocks[0]).toEqual({ kind: "toolCall", callId: "c9", name: "bash", argsJson: `{"cmd":"ls"}` });
});

test("large thinking text → truncated at TRUNC_CAP with fullLen; fullBlockText returns full", () => {
  const big = "z".repeat(TRUNC_CAP + 500);
  const line = `{"type":"message","id":"t","parentId":"p","timestamp":"2026-01-01T00:00:00Z","message":{"role":"assistant","content":[${JSON.stringify({ type: "thinking", thinking: big })}]}}`;
  const m = parseOmpLine(line);
  const b = block(m!, 0, "thinking");
  expect(b.truncated).toBe(true);
  expect(b.fullLen).toBe(TRUNC_CAP + 500);
  expect(b.text.length).toBe(TRUNC_CAP);
  expect(fullBlockText(line, 0)).toBe(big);
});

test("large toolResult text → truncated; fullBlockText returns full", () => {
  const big = "q".repeat(TRUNC_CAP + 123);
  const line = `{"type":"message","id":"r","parentId":"p","timestamp":"2026-01-01T00:00:00Z","message":{"role":"toolResult","toolCallId":"c","toolName":"read","content":[${JSON.stringify({ type: "text", text: big })}],"isError":false}}`;
  const m = parseOmpLine(line);
  const b = block(m!, 0, "toolResult");
  expect(b.truncated).toBe(true);
  expect(b.fullLen).toBe(TRUNC_CAP + 123);
  expect(fullBlockText(line, 0)).toBe(big);
});

test("fullBlockText returns argsJson for a toolCall block, aligned by index", () => {
  // block 0 = thinking, block 1 = toolCall — index must match the streamed order.
  expect(fullBlockText(REAL.asstThinkTool, 0)).toBe("Let me start by reading the plan file as instructed.");
  expect(JSON.parse(fullBlockText(REAL.asstThinkTool, 1)!)).toEqual({ path: "local://price-refresh-july2026-plan.md", i: "Reading authoritative plan" });
  expect(fullBlockText(REAL.asstThinkTool, 9)).toBeNull();
});

// ── Live-corpus smoke: the end-to-end proof on a real transcript ─────────────
const CORPUS = join(process.env.HOME ?? "", ".omp/agent/sessions/-Code-idea");
test.skipIf(!existsSync(CORPUS))("live corpus: a real transcript parses; compaction + toolEvent surface", () => {
  const files = readdirSync(CORPUS).filter((f) => f.endsWith(".jsonl"));
  expect(files.length).toBeGreaterThan(0);
  const lines = readFileSync(join(CORPUS, files[0]!), "utf8").split("\n").filter((l) => l.length > 0);

  let messages = 0;
  let compacted = 0;
  let toolEvents = 0;
  for (const line of lines) {
    const m = parseOmpLine(line); // must never throw
    if (!m) continue;
    messages++;
    if (m.role === "developer" && m.blocks.some((b) => b.kind === "text" && b.text.includes("compacted"))) compacted++;
    if (m.blocks.some((b) => b.kind === "toolEvent")) toolEvents++;
  }
  expect(messages).toBeGreaterThan(0);

  // If the raw transcript contains a compaction / tool_execution_start line, the
  // adapter MUST surface it (the motivating drift bug: compaction was dead).
  const rawHasCompaction = lines.some((l) => l.includes(`"type":"compaction"`));
  const rawHasToolStart = lines.some((l) => l.includes(`"customType":"tool_execution_start"`));
  if (rawHasCompaction) expect(compacted).toBeGreaterThan(0);
  if (rawHasToolStart) expect(toolEvents).toBeGreaterThan(0);
});
