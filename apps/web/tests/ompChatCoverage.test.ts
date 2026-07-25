// Render-parity floor: the omp chat thread must surface EVERY block a transcript
// holds — the "no information lost vs the terminal" contract. analyzeCoverage()
// encodes the one routing rule set OmpChatPane follows; this asserts it drops
// nothing across the tricky cases (image inside a toolResult message, a call-less
// orphan result, a lone tool_event, inline image) and across the five rows the
// TUI paints that the wire only learned to carry with the parity rebuild —
// notice, summary, custom, exec, fileMention — plus a synthetic user turn.

import { test, expect } from "bun:test";
import type { ChatMessage } from "@roost/shared/chat/wire";
import { chatFrameToProto } from "@roost/shared/chat/wire";
import { analyzeCoverage, buildToolIndex, orphanCallIds } from "../src/components/chat/omp/renderPlan.ts";
import { applyOmpChatFrame, ompChatForSession } from "../src/store/chatOmp.ts";

const thread: ChatMessage[] = [
  { id: "u1", parentId: "", ts: "t", role: "user", synthetic: false, blocks: [{ kind: "text", text: "do it" }] },
  { id: "a1", parentId: "u1", ts: "t", role: "assistant", synthetic: false, blocks: [
    { kind: "thinking", text: "plan", truncated: false, fullLen: 4 },
    { kind: "text", text: "reading" },
    { kind: "toolCall", callId: "r", name: "read", argsJson: '{"path":"a.ts"}' },
  ] },
  { id: "tr1", parentId: "a1", ts: "t", role: "toolResult", synthetic: false, blocks: [
    { kind: "toolResult", callId: "r", name: "read", text: "", isError: false, truncated: false, fullLen: 9, rawJson: '{"toolCallId":"r","toolName":"read","content":[{"type":"text","text":"file body"}]}' },
  ] },
  { id: "a2", parentId: "tr1", ts: "t", role: "assistant", synthetic: false, blocks: [
    { kind: "toolCall", callId: "b", name: "browser", argsJson: "{}" },
  ] },
  // toolResult message carrying an IMAGE (was silently dropped pre-fix).
  { id: "tr2", parentId: "a2", ts: "t", role: "toolResult", synthetic: false, blocks: [
    { kind: "toolResult", callId: "b", name: "browser", text: "", isError: false, truncated: false, fullLen: 4, rawJson: '{"toolCallId":"b","toolName":"browser","content":[{"type":"text","text":"shot"}]}' },
    { kind: "image", blobPath: "~/.omp/agent/blobs/deadbeef", mime: "image/webp" },
    { kind: "text", text: "meta" },
  ] },
  // truncated result — recoverable via fetch, still counts as covered.
  { id: "a3", parentId: "tr2", ts: "t", role: "assistant", synthetic: false, blocks: [
    { kind: "toolCall", callId: "e", name: "edit", argsJson: '{"input":"[a.ts#1]"}' },
  ] },
  { id: "tr3", parentId: "a3", ts: "t", role: "toolResult", synthetic: false, blocks: [
    { kind: "toolResult", callId: "e", name: "edit", text: "", isError: false, truncated: true, fullLen: 9000, rawJson: '{"toolCallId":"e","toolName":"edit","content":[{"type":"text","text":"x"}]}' },
  ] },
  // ORPHAN result: no matching toolCall anywhere → own card.
  { id: "tr4", parentId: "a3", ts: "t", role: "toolResult", synthetic: false, blocks: [
    { kind: "toolResult", callId: "orphan", name: "bash", text: "", isError: false, truncated: false, fullLen: 3, rawJson: '{"toolCallId":"orphan","toolName":"bash","content":[{"type":"text","text":"out"}]}' },
  ] },
  // Lone tool_event (no call, no result) → standalone running card.
  { id: "a4", parentId: "tr4", ts: "t", role: "assistant", synthetic: false, blocks: [
    { kind: "toolEvent", callId: "ev", name: "grep", phase: "start", intent: "searching", output: "" },
  ] },
  // Compaction rollup — a collapsible summary card, not the old text divider.
  { id: "d1", parentId: "a4", ts: "t", role: "developer", synthetic: false, blocks: [
    { kind: "summary", variant: "compaction", text: "what came before", tokensBefore: 181416, truncated: false, fullLen: 16 },
  ] },
  // inline image on an assistant message (rendered directly, not via a card).
  { id: "a5", parentId: "d1", ts: "t", role: "assistant", synthetic: false, blocks: [{ kind: "image", blobPath: "data:image/png;base64,AAA", mime: "image/png" }] },
  // Turn-ending error line: omp appends it to the assistant message itself.
  { id: "a6", parentId: "a5", ts: "t", role: "assistant", synthetic: false, blocks: [
    { kind: "text", text: "trying" },
    { kind: "notice", text: "upstream 500", level: "error" },
  ] },
  // Extension-injected card — the row that used to read as a bare `developer` chip.
  { id: "d2", parentId: "a6", ts: "t", role: "developer", synthetic: false, blocks: [
    { kind: "custom", customType: "advisor", text: "**Session update**", detailsJson: "", truncated: false, fullLen: 18 },
  ] },
  // `!cmd` bash block run from omp's composer.
  { id: "d3", parentId: "d2", ts: "t", role: "developer", synthetic: false, blocks: [
    { kind: "exec", lang: "bash", command: "bun test", output: "1 pass", exitCode: 0, cancelled: false, excluded: false, truncated: false, fullLen: 6 },
  ] },
  // `@path` mentions attached to a prompt.
  { id: "d4", parentId: "d3", ts: "t", role: "developer", synthetic: false, blocks: [
    { kind: "fileMention", paths: ["src/a.ts", "src/b.ts"] },
  ] },
  // Agent-attributed user input: collapsed in the pane, still fully covered.
  { id: "u2", parentId: "d4", ts: "t", role: "user", synthetic: true, blocks: [
    { kind: "text", text: "Session update: 3 agents idle" },
  ] },
];

test("coverage: every block reaches the screen — nothing dropped", () => {
  const { total, dropped } = analyzeCoverage(thread);
  expect(dropped).toEqual([]);
  expect(total).toBe(thread.reduce((n, m) => n + m.blocks.length, 0));
});

// The "nothing dropped" assertion is only as strong as what the fixture holds:
// a kind absent here passes trivially, which is exactly how the five TUI rows
// stayed invisible for so long. Pin the reach.
test("the fixture exercises every block kind the TUI paints", () => {
  const kinds = new Set(thread.flatMap((m) => m.blocks.map((b) => b.kind)));
  for (const k of ["notice", "summary", "custom", "exec", "fileMention"] as const) expect(kinds).toContain(k);
  expect(thread.some((m) => m.role === "user" && m.synthetic)).toBe(true);
});

test("toolResult image folds into its call's card (the pre-fix silent-drop bug)", () => {
  const idx = buildToolIndex(thread);
  expect(idx.get("b")!.images).toHaveLength(1);
  expect(idx.get("b")!.images[0]!.blobPath).toBe("~/.omp/agent/blobs/deadbeef");
});

test("call-less results/events are orphans (rendered at their own site)", () => {
  const idx = buildToolIndex(thread);
  const orphans = orphanCallIds(idx).sort();
  expect(orphans).toEqual(["ev", "orphan"]);
  expect(idx.get("orphan")!.call).toBeUndefined();
  expect(idx.get("orphan")!.results).toHaveLength(1);
});

test("result full-text fetch targets the RESULT's coordinates, not the call's", () => {
  const idx = buildToolIndex(thread);
  const m = idx.get("e")!;
  // call is on a3; result is on tr3 — the fetch must use the result's message.
  expect(m.callMsgId).toBe("a3");
  expect(m.results[0]!.msgId).toBe("tr3");
  expect(m.results[0]!.block.truncated).toBe(true);
});

// ─── store projection: upsert-by-id + turn state ─────────────────────────
// The wire is append-OR-REPLACE: a streaming message is re-emitted under the
// same id as it grows. The old skip-by-id splice froze the first token forever.

const SID = "sess-upsert";
type PushOpts = {
  reset?: boolean; streaming?: boolean; model?: string; modelName?: string; thinkingLevel?: string;
  contextTokens?: number; contextWindow?: number; mode?: string;
  /** "" = a frame carrying no status block; both real producers name themselves. */
  engine?: string;
};
const push = (append: ChatMessage[], seq: number, opts: PushOpts = {}) =>
  applyOmpChatFrame(chatFrameToProto({
    sessionId: SID, append, seq, reset: opts.reset ?? false, streaming: opts.streaming ?? false,
    model: opts.model ?? "", modelName: opts.modelName ?? "", thinkingLevel: opts.thinkingLevel ?? "",
    contextTokens: opts.contextTokens ?? 0, contextWindow: opts.contextWindow ?? 0,
    mode: opts.mode ?? "", engine: opts.engine ?? "rpc",
  }));

test("same message id upserts in place — second frame's blocks win", () => {
  push([], 0, { reset: true });
  push([{ id: "m1", parentId: "", ts: "t", role: "assistant", synthetic: false, blocks: [{ kind: "text", text: "Hel" }] }], 1);
  push([{ id: "m1", parentId: "", ts: "t", role: "assistant", synthetic: false, blocks: [{ kind: "text", text: "Hello, world" }] }], 2);

  const state = ompChatForSession(SID);
  expect(state.messages).toHaveLength(1);
  expect(state.messages[0]!.blocks[0]).toEqual({ kind: "text", text: "Hello, world" });
  expect(state.seq).toBe(2);

  // An unknown id still appends, after the upserted row.
  push([{ id: "m2", parentId: "m1", ts: "t", role: "user", synthetic: false, blocks: [{ kind: "text", text: "next" }] }], 3);
  expect(ompChatForSession(SID).messages.map((m) => m.id)).toEqual(["m1", "m2"]);
});

// NOTE: the streaming row-identity contract (keyed reconcile keeps `<For>` from
// remounting a growing message) CANNOT be asserted here — `bun test` resolves
// solid-js/store to its SERVER build, whose `reconcile` is a plain replace with
// no identity preservation. It is proven in the browser instead, by
// step15_chat_stream_round_trip in .claude/skills/roost-smoke/run.js, which
// asserts the assistant bubble's DOM node survives its own text growing.

test("ChatFrame.streaming lands on chat_omp[sid].streaming, payload or not", () => {
  push([], 0, { reset: true });
  expect(ompChatForSession(SID).streaming).toBe(false);

  // Payload-less turn-state frame (the worker's agent_start) must still apply.
  push([], 0, { streaming: true });
  expect(ompChatForSession(SID).streaming).toBe(true);

  push([{ id: "m1", parentId: "", ts: "t", role: "assistant", synthetic: false, blocks: [{ kind: "text", text: "hi" }] }], 1, { streaming: true });
  expect(ompChatForSession(SID).streaming).toBe(true);

  push([], 1, { streaming: false });
  expect(ompChatForSession(SID).streaming).toBe(false);
});

test("reset keeps the LAST copy of a repeated id, not the first", () => {
  push([
    { id: "m1", parentId: "", ts: "t", role: "assistant", synthetic: false, blocks: [{ kind: "text", text: "partial" }] },
    { id: "m1", parentId: "", ts: "t", role: "assistant", synthetic: false, blocks: [{ kind: "text", text: "complete" }] },
  ], 9, { reset: true });
  const state = ompChatForSession(SID);
  expect(state.messages).toHaveLength(1);
  expect(state.messages[0]!.blocks[0]).toEqual({ kind: "text", text: "complete" });
});

test("session status rides payload-less frames onto the store", () => {
  push([], 0, { reset: true });
  expect(ompChatForSession(SID).model).toBe("");

  // agent_end sends no messages and no seq bump — the status must still land,
  // or the header freezes at whatever the first frame happened to carry.
  // Every status field crosses BOTH proto adapter arms here (push encodes,
  // applyOmpChatFrame decodes) — one omitted from either arm drops silently.
  push([], 0, { model: "anthropic/claude-opus-5", contextTokens: 20_000, contextWindow: 1_000_000, mode: "plan", engine: "mirror" });
  expect(ompChatForSession(SID).model).toBe("anthropic/claude-opus-5");
  expect(ompChatForSession(SID).contextTokens).toBe(20_000);
  expect(ompChatForSession(SID).contextWindow).toBe(1_000_000);
  expect(ompChatForSession(SID).mode).toBe("plan");
  expect(ompChatForSession(SID).engine).toBe("mirror");
});

test("model name + thinking level ride the frame; a status-less frame can't clobber them", () => {
  push([], 0, { reset: true });

  push([], 0, { model: "anthropic/claude-sonnet-5", modelName: "Claude Sonnet 5", thinkingLevel: "medium" });
  expect(ompChatForSession(SID).modelName).toBe("Claude Sonnet 5");
  expect(ompChatForSession(SID).thinkingLevel).toBe("medium");

  // A frame built before its producer resolved any status names no engine and
  // carries empty fields. It must not blank what a real status frame set.
  push([], 0, { engine: "", model: "", modelName: "", thinkingLevel: "" });
  expect(ompChatForSession(SID).modelName).toBe("Claude Sonnet 5");
  expect(ompChatForSession(SID).thinkingLevel).toBe("medium");
});

test("a selection card survives the proto boundary intact", () => {
  // The ask render model is rebuilt in the WORKER and only reaches the pane
  // through proto. A field added to the zod schema but not to both adapter arms
  // vanishes here silently, and the card degrades to a label-only button row.
  push([], 0, { reset: true });
  push([{
    id: "ap1", parentId: "", ts: "t", role: "developer", synthetic: false, blocks: [{
      kind: "approval", requestId: "ui-9", method: "select",
      title: "Which features?", message: "", resolved: false, answer: "",
      options: ["Streaming", "Search", "Next →"],
      header: "Scope", progress: "2/3", multi: true,
      richOptions: [
        { value: "Streaming", label: "Streaming", description: "Token by token.", recommended: true, checked: true, role: "option" },
        { value: "Search", label: "Search", description: "", recommended: false, checked: false, role: "option" },
        { value: "Next →", label: "Next →", description: "", recommended: false, checked: false, role: "next" },
      ],
    }],
  }], 1);

  const block = ompChatForSession(SID).messages[0]!.blocks[0]!;
  expect(block.kind).toBe("approval");
  if (block.kind !== "approval") return;
  expect(block.header).toBe("Scope");
  expect(block.progress).toBe("2/3");
  expect(block.multi).toBe(true);
  expect(block.richOptions).toHaveLength(3);
  expect(block.richOptions[0]).toEqual({
    value: "Streaming", label: "Streaming", description: "Token by token.",
    recommended: true, checked: true, role: "option",
  });
  expect(block.richOptions[2]!.role).toBe("next");
  // `options` keeps its own meaning — the raw frame echo the smoke path reads.
  expect(block.options).toEqual(["Streaming", "Search", "Next →"]);
});

test("tool index exposes the newest event, including live update output", () => {
  const call: ChatMessage = { id: "a", parentId: "", ts: "t", role: "assistant", synthetic: false, blocks: [
    { kind: "toolCall", callId: "c1", name: "bash", argsJson: "{}" },
  ] };
  const evStart: ChatMessage = { id: "e", parentId: "a", ts: "t", role: "assistant", synthetic: false, blocks: [
    { kind: "toolEvent", callId: "c1", name: "bash", phase: "start", intent: "", output: "" },
  ] };
  // Same message id, replaced in place as the tool streams — the index must
  // surface the newest block, not the one present when the card first mounted.
  const evUpdate: ChatMessage = { ...evStart, blocks: [
    { kind: "toolEvent", callId: "c1", name: "bash", phase: "update", intent: "", output: "tick 1\ntick 2\n" },
  ] };

  const before = buildToolIndex([call, evStart]);
  expect(before.get("c1")!.event!.phase).toBe("start");
  expect(before.get("c1")!.event!.output).toBe("");

  const after = buildToolIndex([call, evUpdate]);
  expect(after.get("c1")!.event!.phase).toBe("update");
  expect(after.get("c1")!.event!.output).toBe("tick 1\ntick 2\n");

  // An update-phase event still counts as covered routing (folds into the call).
  expect(analyzeCoverage([call, evUpdate]).dropped).toEqual([]);
});
