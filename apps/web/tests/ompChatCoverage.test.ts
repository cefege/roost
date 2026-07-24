// Render-parity floor: the omp chat thread must surface EVERY block a transcript
// holds — the "no information lost vs the terminal" contract. analyzeCoverage()
// encodes the one routing rule set OmpChatPane follows; this asserts it drops
// nothing across the tricky cases (image inside a toolResult message, a call-less
// orphan result, a lone tool_event, inline image, compaction divider) — exactly
// the classes that were silently dropped before the parity rebuild.

import { test, expect } from "bun:test";
import type { ChatMessage } from "@roost/shared/chat/wire";
import { analyzeCoverage, buildToolIndex, orphanCallIds } from "../src/components/chat/omp/renderPlan.ts";

const thread: ChatMessage[] = [
  { id: "u1", parentId: "", ts: "t", role: "user", blocks: [{ kind: "text", text: "do it" }] },
  { id: "a1", parentId: "u1", ts: "t", role: "assistant", blocks: [
    { kind: "thinking", text: "plan", truncated: false, fullLen: 4 },
    { kind: "text", text: "reading" },
    { kind: "toolCall", callId: "r", name: "read", argsJson: '{"path":"a.ts"}' },
  ] },
  { id: "tr1", parentId: "a1", ts: "t", role: "toolResult", blocks: [
    { kind: "toolResult", callId: "r", name: "read", text: "file body", isError: false, truncated: false, fullLen: 9 },
  ] },
  { id: "a2", parentId: "tr1", ts: "t", role: "assistant", blocks: [
    { kind: "toolCall", callId: "b", name: "browser", argsJson: "{}" },
  ] },
  // toolResult message carrying an IMAGE (was silently dropped pre-fix).
  { id: "tr2", parentId: "a2", ts: "t", role: "toolResult", blocks: [
    { kind: "toolResult", callId: "b", name: "browser", text: "shot", isError: false, truncated: false, fullLen: 4 },
    { kind: "image", blobPath: "~/.omp/agent/blobs/deadbeef", mime: "image/webp" },
    { kind: "text", text: "meta" },
  ] },
  // truncated result — recoverable via fetch, still counts as covered.
  { id: "a3", parentId: "tr2", ts: "t", role: "assistant", blocks: [
    { kind: "toolCall", callId: "e", name: "edit", argsJson: '{"input":"[a.ts#1]"}' },
  ] },
  { id: "tr3", parentId: "a3", ts: "t", role: "toolResult", blocks: [
    { kind: "toolResult", callId: "e", name: "edit", text: "x".repeat(8192), isError: false, truncated: true, fullLen: 9000 },
  ] },
  // ORPHAN result: no matching toolCall anywhere → own card.
  { id: "tr4", parentId: "a3", ts: "t", role: "toolResult", blocks: [
    { kind: "toolResult", callId: "orphan", name: "bash", text: "out", isError: false, truncated: false, fullLen: 3 },
  ] },
  // Lone tool_event (no call, no result) → standalone running card.
  { id: "a4", parentId: "tr4", ts: "t", role: "assistant", blocks: [
    { kind: "toolEvent", callId: "ev", name: "grep", phase: "start", intent: "searching" },
  ] },
  { id: "d1", parentId: "a4", ts: "t", role: "developer", blocks: [{ kind: "text", text: "— context compacted —" }] },
  // inline image on an assistant message (rendered directly, not via a card).
  { id: "a5", parentId: "d1", ts: "t", role: "assistant", blocks: [{ kind: "image", blobPath: "data:image/png;base64,AAA", mime: "image/png" }] },
];

test("coverage: every block reaches the screen — nothing dropped", () => {
  const { total, dropped } = analyzeCoverage(thread);
  expect(dropped).toEqual([]);
  expect(total).toBe(thread.reduce((n, m) => n + m.blocks.length, 0));
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
