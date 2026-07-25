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
import { RAW_CAP, type ChatMessage, type ContentBlock } from "@roost/shared/chat/wire";
import { parseOmpLine, parseOmpStatusDelta, fullBlockText, TRUNC_CAP } from "../../../src/chat/omp/parse.ts";

/** Narrow blocks[i] to a concrete kind (checked — throws if absent/mismatched). */
function block<K extends ContentBlock["kind"]>(m: ChatMessage, i: number, kind: K): Extract<ContentBlock, { kind: K }> {
  const b = m.blocks[i];
  if (b === undefined) throw new Error(`no block at index ${i}`);
  if (b.kind !== kind) throw new Error(`block ${i}: expected ${kind}, got ${b.kind}`);
  return b as Extract<ContentBlock, { kind: K }>;
}

/** The omp ToolResultMessage envelope carried in ToolResultBlock.rawJson. Shape
 *  is pinned by the parser under test, so the tests read it as this. */
type RawEnvelope = {
  toolCallId: string; toolName: string; isError: boolean;
  content: { type: string; text: string }[]; details?: unknown;
};
function rawEnvelope(b: Extract<ContentBlock, { kind: "toolResult" }>): RawEnvelope {
  return JSON.parse(b.rawJson) as RawEnvelope;
}

/** Wrap an omp message object in its transcript `message` entry envelope. */
function messageLine(message: Record<string, unknown>, id = "m1"): string {
  return JSON.stringify({ type: "message", id, parentId: "p0", timestamp: "2026-01-01T00:00:00Z", message });
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

test("toolResult error → one toolResult block whose rawJson is the omp envelope", () => {
  const m = parseOmpLine(REAL.toolResultErr);
  expect(m!.role).toBe("toolResult");
  expect(m!.blocks).toHaveLength(1);
  const b = block(m!, 0, "toolResult");
  expect(b).toMatchObject({
    callId: "call_d6dc057e3bbf42cbac09cfd3", name: "grep", isError: true,
    text: "", truncated: false, fullLen: 48,
  });
  // The payload <omp-tool-view> reads: message-level metadata + verbatim content
  // + the per-tool `details` the old flattening dropped.
  expect(JSON.parse(b.rawJson)).toEqual({
    toolCallId: "call_d6dc057e3bbf42cbac09cfd3", toolName: "grep", isError: true,
    content: [{ type: "text", text: "artifact:// ID must be numeric, got: 2;artifact:" }],
    details: {},
  });
});

test("developer message → suppressed, as omp's TUI suppresses it", () => {
  expect(parseOmpLine(REAL.developer)).toBeNull();
});

test("custom tool_execution_start → toolEvent block (role assistant, phase start)", () => {
  const m = parseOmpLine(REAL.toolStart);
  expect(m!.role).toBe("assistant");
  expect(m!.blocks).toEqual([
    { kind: "toolEvent", callId: "call_0167ad99a3704a1d883a8d9e", name: "read", phase: "start", intent: "Reading authoritative plan", output: "" },
  ]);
});

test("top-level compaction entry → collapsible summary card (the drift bug this fixes)", () => {
  const m = parseOmpLine(REAL.compaction);
  expect(m!.role).toBe("developer");
  expect(m!.blocks).toHaveLength(1);
  const b = block(m!, 0, "summary");
  expect(b.variant).toBe("compaction");
  expect(b.tokensBefore).toBe(181416);
  expect(b.text).toBe("[Superseded compaction summary elided after a newer compaction]");
  expect(b.truncated).toBe(false);
});

test("custom_message display:true → labelled custom card", () => {
  const m = parseOmpLine(REAL.cmVisible);
  expect(m!.role).toBe("developer");
  expect(m!.blocks).toHaveLength(1);
  const b = block(m!, 0, "custom");
  expect(b.customType).toBe("advisor");
  expect(b.text).toBe("heads up: verify the scrape");
  expect(b.detailsJson).toBe("");
  expect(b.fullLen).toBe(b.text.length);
});

test("custom_message display:false → null (omp marks it hidden)", () => {
  expect(parseOmpLine(REAL.cmHidden)).toBeNull();
});

test("branch_summary → collapsible branch summary card", () => {
  const m = parseOmpLine(REAL.branch);
  expect(m!.role).toBe("developer");
  expect(m!.blocks).toEqual([{
    kind: "summary", variant: "branch", text: "implemented the feature",
    tokensBefore: 0, truncated: false, fullLen: 23,
  }]);
});

test("image block blob:sha256 → one toolResult (both texts joined in rawJson) + image", () => {
  const m = parseOmpLine(REAL.image);
  expect(m!.blocks.map((b) => b.kind)).toEqual(["toolResult", "image"]);
  const raw = rawEnvelope(block(m!, 0, "toolResult"));
  expect(raw.content.map((c) => c.text)).toEqual(["Screenshot captured", "done"]);
  const img = block(m!, 1, "image");
  expect(img.blobPath).toBe(`${process.env.HOME}/.omp/agent/blobs/edc40f82d28389d5a5873f030f59b4e2edc9b5057f2c2465016afea334ef5cb3`);
  expect(img.mime).toBe("image/webp");
});

test("unknown custom type → null (never throws)", () => {
  expect(parseOmpLine(`{"type":"custom","customType":"session_exit","data":{},"id":"c","parentId":"p","timestamp":"2026-01-01T00:00:00Z"}`)).toBeNull();
});

test("unknown message role → null", () => {
  expect(parseOmpLine(messageLine({ role: "somethingNew", content: [{ type: "text", text: "x" }] }))).toBeNull();
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

test("large toolResult text → truncated flags set; rawJson still carries it whole", () => {
  const big = "q".repeat(TRUNC_CAP + 123);
  const line = `{"type":"message","id":"r","parentId":"p","timestamp":"2026-01-01T00:00:00Z","message":{"role":"toolResult","toolCallId":"c","toolName":"read","content":[${JSON.stringify({ type: "text", text: big })}],"isError":false}}`;
  const b = block(parseOmpLine(line)!, 0, "toolResult");
  expect(b.truncated).toBe(true);
  expect(b.fullLen).toBe(TRUNC_CAP + 123);
  // Under RAW_CAP, so nothing is cut — the block's own `text` stays empty.
  expect(b.text).toBe("");
  expect(rawEnvelope(b).content[0]!.text).toBe(big);
  expect(fullBlockText(line, 0)).toBeNull();
});

test("toolResult over RAW_CAP → valid JSON, details dropped, text capped", () => {
  const big = "w".repeat(RAW_CAP + 10);
  const line = `{"type":"message","id":"r2","parentId":"p","timestamp":"2026-01-01T00:00:00Z","message":{"role":"toolResult","toolCallId":"c2","toolName":"bash","content":[${JSON.stringify({ type: "text", text: big })}],"details":{"wallTimeMs":12},"isError":false}}`;
  const b = block(parseOmpLine(line)!, 0, "toolResult");
  expect(b.rawJson.length).toBeLessThanOrEqual(RAW_CAP);
  const raw = rawEnvelope(b);
  expect(raw.details).toBeUndefined();
  expect(raw.content).toHaveLength(1);
  expect(raw.content[0]!.text.startsWith("w".repeat(TRUNC_CAP))).toBe(true);
  expect(raw.content[0]!.text.endsWith(`… (truncated, ${RAW_CAP + 10 - TRUNC_CAP} more characters)`)).toBe(true);
});

test("oversized details with short text → capped envelope keeps the text verbatim", () => {
  const fat = { blob: "d".repeat(RAW_CAP + 10) };
  const line = `{"type":"message","id":"r3","parentId":"p","timestamp":"2026-01-01T00:00:00Z","message":{"role":"toolResult","toolCallId":"c3","toolName":"edit","content":[{"type":"text","text":"ok"}],"details":${JSON.stringify(fat)},"isError":false}}`;
  const raw = rawEnvelope(block(parseOmpLine(line)!, 0, "toolResult"));
  expect(raw.details).toBeUndefined();
  expect(raw.content[0]!.text).toBe("ok");
});

test("fullBlockText returns argsJson for a toolCall block, aligned by index", () => {
  // block 0 = thinking, block 1 = toolCall — index must match the streamed order.
  expect(fullBlockText(REAL.asstThinkTool, 0)).toBe("Let me start by reading the plan file as instructed.");
  expect(JSON.parse(fullBlockText(REAL.asstThinkTool, 1)!)).toEqual({ path: "local://price-refresh-july2026-plan.md", i: "Reading authoritative plan" });
  expect(fullBlockText(REAL.asstThinkTool, 9)).toBeNull();
});

// ── omp TUI parity: the rows the terminal paints the pane used to drop ───────
// Error semantics are a port of omp 17.1.3's resolveAssistantErrorPresentation;
// the errorId literals are real AIError flag bitmasks (Class|SilentAbort etc.)
// captured from the live corpus.

test("aborted turn interrupted by the user → NO notice row (the TUI stays quiet)", () => {
  const line = messageLine({
    role: "assistant", content: [{ type: "text", text: "partial" }],
    stopReason: "aborted", errorMessage: "Interrupted by user", errorId: 67112960,
  });
  expect(parseOmpLine(line)!.blocks.map((b) => b.kind)).toEqual(["text"]);
});

test("silent-abort marker → NO notice row", () => {
  const line = messageLine({
    role: "assistant", content: [{ type: "text", text: "partial" }],
    stopReason: "aborted", errorMessage: "__omp.silent_abort__", errorId: 33558528,
  });
  expect(parseOmpLine(line)!.blocks.map((b) => b.kind)).toEqual(["text"]);
});

test("bare abort (Abort flag, sentinel reason) → 'Operation aborted', even with no content", () => {
  const line = messageLine({
    role: "assistant", content: [],
    stopReason: "aborted", errorMessage: "Request was aborted", errorId: 134221824,
  });
  expect(parseOmpLine(line)!.blocks).toEqual([{ kind: "notice", level: "error", text: "Operation aborted" }]);
});

test("abort with a custom reason → the reason verbatim", () => {
  const line = messageLine({ role: "assistant", content: [], stopReason: "aborted", errorMessage: "advisor reset" });
  expect(parseOmpLine(line)!.blocks).toEqual([{ kind: "notice", level: "error", text: "advisor reset" }]);
});

test("stopReason error → exactly one notice block, appended last", () => {
  const line = messageLine({
    role: "assistant", content: [{ type: "text", text: "sorry" }],
    stopReason: "error", errorMessage: "upstream 500",
  });
  expect(parseOmpLine(line)!.blocks).toEqual([
    { kind: "text", text: "sorry" },
    { kind: "notice", level: "error", text: "upstream 500" },
  ]);
});

test("errored turn WITH a tool call → still ONE notice, no synthesized toolResult", () => {
  // Deliberate divergence from omp, which folds the error line into each tool
  // card: synthesizing a toolResult risks duplicating the call's real result,
  // and the shape fires on 6 of 39_155 assistant messages in the live corpus.
  const line = messageLine({
    role: "assistant", stopReason: "error", errorMessage: "upstream 500",
    content: [{ type: "toolCall", id: "c1", name: "read", arguments: { path: "x" } }],
  });
  const m = parseOmpLine(line);
  expect(m!.blocks.map((b) => b.kind)).toEqual(["toolCall", "notice"]);
  expect(block(m!, 1, "notice")).toEqual({ kind: "notice", level: "error", text: "upstream 500" });
});

test("recovered auto-retry → a dim `note`, whitespace collapsed", () => {
  const line = messageLine({
    role: "assistant", content: [{ type: "text", text: "ok" }],
    retryRecovery: { kind: "auto-retry", status: "recovered", attempt: 1, note: "error;\n  retried" },
  });
  expect(block(parseOmpLine(line)!, 1, "notice")).toEqual({ kind: "notice", level: "note", text: "error; retried" });
});

test("synthetic user turn → ChatMessage.synthetic (omp collapses these)", () => {
  const line = messageLine({ role: "user", content: [{ type: "text", text: "Session update…" }], synthetic: true });
  expect(parseOmpLine(line)!.synthetic).toBe(true);
  expect(parseOmpLine(REAL.userText)!.synthetic).toBe(false);
});

test("user content as a bare string → one text block (omp's userMessageText)", () => {
  const line = messageLine({ role: "user", content: "your interruptible wait was interrupted" });
  expect(parseOmpLine(line)!.blocks).toEqual([{ kind: "text", text: "your interruptible wait was interrupted" }]);
});

test("bashExecution → developer exec block carrying command, output and exit code", () => {
  const line = messageLine({
    role: "bashExecution", command: "ls -la", output: "a\nb",
    exitCode: 0, cancelled: false, excludeFromContext: true,
  });
  const m = parseOmpLine(line);
  expect(m!.role).toBe("developer");
  expect(m!.blocks).toEqual([{
    kind: "exec", lang: "bash", command: "ls -la", output: "a\nb",
    exitCode: 0, cancelled: false, excluded: true, truncated: false, fullLen: 3,
  }]);
});

test("pythonExecution → exec block reading `code`; a missing exitCode is -1", () => {
  const line = messageLine({ role: "pythonExecution", code: "print(1)", output: "1\n", cancelled: true });
  const b = block(parseOmpLine(line)!, 0, "exec");
  expect(b.lang).toBe("python");
  expect(b.command).toBe("print(1)");
  expect(b.exitCode).toBe(-1);
  expect(b.cancelled).toBe(true);
});

test("fileMention → one fileMention block; entries without a path are dropped", () => {
  const line = messageLine({
    role: "fileMention",
    files: [{ path: "/a/b.png", content: "[Image]" }, "/c/d.ts", { content: "no path" }],
  });
  const m = parseOmpLine(line);
  expect(m!.role).toBe("developer");
  expect(m!.blocks).toEqual([{ kind: "fileMention", paths: ["/a/b.png", "/c/d.ts"] }]);
});

test("custom role message → card when display:true (details carried), null otherwise", () => {
  const shown = messageLine({
    role: "custom", customType: "async-result", content: "done", display: true, details: { jobId: 7 },
  });
  const b = block(parseOmpLine(shown)!, 0, "custom");
  expect(b.customType).toBe("async-result");
  expect(b.text).toBe("done");
  expect(JSON.parse(b.detailsJson)).toEqual({ jobId: 7 });
  const hidden = messageLine({ role: "custom", customType: "async-result", content: "done", display: false });
  expect(parseOmpLine(hidden)).toBeNull();
});

test("fullBlockText serves summary / custom / exec text untruncated", () => {
  const big = "s".repeat(TRUNC_CAP + 7);
  const comp = JSON.stringify({ type: "compaction", id: "c", parentId: "p", timestamp: "t", summary: big, tokensBefore: 10 });
  expect(block(parseOmpLine(comp)!, 0, "summary").truncated).toBe(true);
  expect(fullBlockText(comp, 0)).toBe(big);
  const cm = JSON.stringify({ type: "custom_message", id: "c2", parentId: "p", timestamp: "t", customType: "advisor", display: true, content: big });
  expect(fullBlockText(cm, 0)).toBe(big);
  const bash = messageLine({ role: "bashExecution", command: "x", output: big, exitCode: 0 });
  expect(fullBlockText(bash, 0)).toBe(big);
});
// ── parseOmpStatusDelta: the statusline facts, off the same raw lines ────────
// Verbatim captured lines again — the status parser reads the RAW object (the
// SDK typings carry no contextSnapshot), so a shape drift shows up only here.
const STATUS = {
  modelChange: `{"type":"model_change","id":"3942ae7a","parentId":null,"timestamp":"2026-07-25T09:56:48.634Z","model":"anthropic/claude-opus-5"}`,
  modePlan: `{"type":"mode_change","id":"5fc8f8c9","parentId":"3942ae7a","timestamp":"2026-07-25T09:56:48.797Z","mode":"plan","data":{"planFilePath":"local://PLAN.md"}}`,
  modeNone: `{"type":"mode_change","id":"1700dbef","parentId":"6894f1bc","timestamp":"2026-07-25T09:59:58.651Z","mode":"none"}`,
  thinking: `{"type":"thinking_level_change","id":"ff0ead9c","parentId":null,"timestamp":"2026-07-25T09:59:58.657Z","thinkingLevel":"high","configured":"auto"}`,
  assistant: `{"type":"message","id":"511f15fc","parentId":"57546175","timestamp":"2026-07-24T18:53:47.767Z","message":{"role":"assistant","content":[{"type":"text","text":"CLICK PATH OK."}],"api":"anthropic-messages","provider":"anthropic","model":"claude-opus-4-8","usage":{"input":2,"output":12,"cacheRead":34691,"cacheWrite":68479,"totalTokens":103184,"cost":{"input":0.00001,"output":0.00030000000000000003,"cacheRead":0.0173455,"cacheWrite":0.42799375,"total":0.44564925},"cttl":{"ephemeral1h":68479}},"stopReason":"stop","timestamp":1784919224524,"responseId":"msg_011CdMPXQ1W6syzb6QxaogZC","duration":3234.544875000138,"ttft":2970.097208000021,"contextSnapshot":{"promptTokens":103172,"nonMessageTokens":18829}}}`,
};

test("model_change / mode_change / thinking_level_change → one fact each", () => {
  expect(parseOmpStatusDelta(STATUS.modelChange)).toEqual({ model: "anthropic/claude-opus-5" });
  expect(parseOmpStatusDelta(STATUS.modePlan)).toEqual({ mode: "plan" });
  // "none" is what omp writes on EXIT. It is a real fact (it clears the chip),
  // not an absence — dropping it would strand a stale "Plan" on screen.
  expect(parseOmpStatusDelta(STATUS.modeNone)).toEqual({ mode: "none" });
  expect(parseOmpStatusDelta(STATUS.thinking)).toEqual({ thinkingLevel: "high" });
});

test("thinking_level_change with an unresolved level falls back to `configured`", () => {
  const line = `{"type":"thinking_level_change","id":"t1","parentId":null,"timestamp":"2026-01-01T00:00:00Z","thinkingLevel":"","configured":"auto"}`;
  expect(parseOmpStatusDelta(line)).toEqual({ thinkingLevel: "auto" });
});

test("assistant message yields BOTH context tokens and the model fallback", () => {
  // The fallback is load-bearing: omp writes model_change only when the model
  // is SELECTED, so a session that never touched the picker has none at all.
  // Both sources produce the same provider/id shape, so they cannot disagree.
  expect(parseOmpStatusDelta(STATUS.assistant)).toEqual({
    contextTokens: 103172,
    model: "anthropic/claude-opus-4-8",
  });
});

test("non-status lines → null (ill-formed, unknown type, user turn)", () => {
  expect(parseOmpStatusDelta("not json")).toBeNull();
  expect(parseOmpStatusDelta(`{"type":"custom"}`)).toBeNull();
  expect(parseOmpStatusDelta(REAL.userText)).toBeNull();
  // An assistant message with neither a snapshot nor a provider carries nothing.
  expect(parseOmpStatusDelta(`{"type":"message","id":"a","parentId":"p","timestamp":"t","message":{"role":"assistant","content":[]}}`)).toBeNull();
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
    if (m.role === "developer" && m.blocks.some((b) => b.kind === "summary" && b.variant === "compaction")) compacted++;
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
