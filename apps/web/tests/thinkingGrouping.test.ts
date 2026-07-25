// Thinking-run grouping: the transcript renders reasoning the way omp does —
// dots-only blocks dropped (canonicalizeMessage), adjacent blocks read as ONE
// passage. The checked-in fixture holds no dots-only block, so the "a dropped
// block does not split a run" rule has no other cover.

import { test, expect } from "bun:test";
import type { ContentBlock } from "@roost/shared/chat/wire";
import { canonicalizeThinking, groupThinking } from "../src/components/chat/omp/thinkingText.ts";

const think = (text: string): ContentBlock => ({ kind: "thinking", text, truncated: false, fullLen: text.length });
const say = (text: string): ContentBlock => ({ kind: "text", text });

test("canonicalizeThinking drops dots/ellipsis/whitespace-only blocks, trims the rest", () => {
  expect(canonicalizeThinking("  ...  ")).toBe("");
  expect(canonicalizeThinking("…")).toBe("");
  expect(canonicalizeThinking("\n\t . … \n")).toBe("");
  expect(canonicalizeThinking("")).toBe("");
  expect(canonicalizeThinking(undefined)).toBe("");
  expect(canonicalizeThinking("  real thought.  ")).toBe("real thought.");
});

test("adjacent thinking blocks coalesce into one run, in order", () => {
  const out = groupThinking([think("first"), think("second"), say("hi"), think("third")]);
  expect(out.map((r) => r.kind)).toEqual(["thinking", "block", "thinking"]);
  const run = out[0] as Extract<typeof out[number], { kind: "thinking" }>;
  expect(run.parts.map((p) => [p.index, p.block.text])).toEqual([[0, "first"], [1, "second"]]);
  expect(out[1]!.index).toBe(2);
  expect((out[2] as typeof run).parts.map((p) => p.index)).toEqual([3]);
});

test("a dots-only block is dropped and does NOT split the run around it", () => {
  const out = groupThinking([think("before"), think("..."), think("after")]);
  expect(out).toHaveLength(1);
  const run = out[0] as Extract<typeof out[number], { kind: "thinking" }>;
  expect(run.parts.map((p) => p.index)).toEqual([0, 2]);
});

test("a run whose every part is dots-only is not emitted at all", () => {
  expect(groupThinking([think("…"), think("  ")])).toEqual([]);
});

test("non-thinking blocks keep their original block index", () => {
  const out = groupThinking([say("a"), think("t"), say("b")]);
  expect(out.map((r) => r.index)).toEqual([0, 1, 2]);
});
