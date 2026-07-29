// projectRpcFrame is the whole worker-side agent slice in one pure function, so
// it is the one thing worth a unit test: replay a recorded omp frame sequence
// and assert the transcript it folds down to. Frame shapes are verbatim from a
// live omp v17.1.7 child (local://omp-rpc-contract.md) — if omp changes them,
// this test is what notices.

import { test, expect } from "bun:test";
import type { AgentEntry } from "@roost/shared/wire/agent-entry";
import type { RpcFrame } from "../src/agent/rpc-frame.ts";
import {
  applyEntryPatch,
  newProjectionState,
  projectRpcFrame,
} from "../src/agent/entry-projection.ts";

// Pinned so entry timestamps stay deterministic.
const TS = 1_700_000_000_000;

/** Fold a frame sequence exactly the way AgentController does: append to the
 *  ring, patch in place by seq. Sharing applyEntryPatch with production is the
 *  point — a test with its own fold would pass while the real one drifted. */
function fold(frames: RpcFrame[]): AgentEntry[] {
  const state = newProjectionState();
  const bySeq = new Map<number, AgentEntry>();
  const order: number[] = [];
  for (const frame of frames) {
    for (const op of projectRpcFrame(frame, state, TS)) {
      if (op.op === "append") {
        bySeq.set(op.entry.seq, op.entry);
        order.push(op.entry.seq);
        continue;
      }
      const entry = bySeq.get(op.seq);
      if (entry) applyEntryPatch(entry, op.patch);
    }
  }
  return order.map((seq) => {
    const entry = bySeq.get(seq);
    if (!entry) throw new Error(`appended seq ${seq} vanished`);
    return entry;
  });
}

const textFrame = (event: Record<string, unknown>): RpcFrame => ({
  type: "message_update",
  assistantMessageEvent: event,
});

const TOOL_CALL_ID = "toolu_0159ogXpQY";
const TOOL_DETAILS = { timeoutSeconds: 300, wallTimeMs: 239.2 };

const TURN: RpcFrame[] = [
  textFrame({ type: "text_start", contentIndex: 0 }),
  textFrame({ type: "text_delta", contentIndex: 0, delta: "Listing " }),
  textFrame({ type: "text_delta", contentIndex: 0, delta: "the files." }),
  textFrame({ type: "text_end", contentIndex: 0, content: "Listing the files." }),
  {
    type: "tool_execution_start",
    toolCallId: TOOL_CALL_ID,
    toolName: "bash",
    args: { command: "ls -1" },
    intent: "Listing directory",
  },
  {
    type: "tool_execution_update",
    toolCallId: TOOL_CALL_ID,
    toolName: "bash",
    partialResult: { content: [{ type: "text", text: "hello.txt\n" }], details: {} },
  },
  {
    type: "tool_execution_end",
    toolCallId: TOOL_CALL_ID,
    toolName: "bash",
    result: {
      content: [{ type: "text", text: "hello.txt\n\n\nWall time: 0.24 seconds" }],
      details: TOOL_DETAILS,
    },
    isError: false,
  },
  {
    type: "extension_ui_request",
    id: "1542ec2f4aa95b3e",
    method: "select",
    title: "Allow tool: bash\nCommand: ls -1",
    options: ["Approve", "Deny"],
  },
];

test("a streamed turn folds to one assistant, one tool and one approval entry", () => {
  const entries = fold(TURN);
  expect(entries.map((e) => e.kind)).toEqual(["assistant", "tool", "prompt"]);
  // seq is monotonic from 1 — the client upserts on it, so a gap or a reset
  // would silently reorder the transcript.
  expect(entries.map((e) => e.seq)).toEqual([1, 2, 3]);

  const assistant = entries[0];
  if (assistant?.kind !== "assistant") throw new Error("expected an assistant entry");
  expect(assistant.text).toBe("Listing the files.");
  expect(assistant.done).toBe(true);

  const tool = entries[1];
  if (tool?.kind !== "tool") throw new Error("expected a tool entry");
  expect(tool.tool_call_id).toBe(TOOL_CALL_ID);
  expect(tool.name).toBe("bash");
  expect(tool.status).toBe("ok");
  expect(tool.intent).toBe("Listing directory");
  expect(tool.text).toBe("hello.txt\n\n\nWall time: 0.24 seconds");
  // details_json is contract-typed as parseable JSON; the web tool card parses
  // it for the edit diff.
  expect(JSON.parse(tool.details_json)).toEqual(TOOL_DETAILS);
  expect(JSON.parse(tool.args_json)).toEqual({ command: "ls -1" });

  const prompt = entries[2];
  if (prompt?.kind !== "prompt") throw new Error("expected a prompt entry");
  expect(prompt.prompt_kind).toBe("approval");
  expect(prompt.state).toBe("pending");
  expect(prompt.prompt_id).toBe("1542ec2f4aa95b3e");
  expect(prompt.options).toEqual(["Approve", "Deny"]);
});

test("deltas accumulate before the end frame arrives", () => {
  const entries = fold(TURN.slice(0, 3));
  const assistant = entries[0];
  if (assistant?.kind !== "assistant") throw new Error("expected an assistant entry");
  expect(assistant.text).toBe("Listing the files.");
  // Still open: the composer must not render it as a finished message.
  expect(assistant.done).toBe(false);
});

test("chrome extension_ui_requests never become prompts", () => {
  // THE TRAP: setWidget arrives as the same frame type as a real approval but
  // carries no title/options and needs no reply. Matching on the frame type
  // alone would put an unanswerable card in the transcript and pin the session
  // at needs-input forever.
  const entries = fold([
    { type: "extension_ui_request", id: "1542eb00", method: "setWidget", widgetKey: "autoresearch" },
  ]);
  expect(entries.filter((e) => e.kind === "prompt")).toEqual([]);
  expect(entries).toEqual([]);
});

test("a non-approval select is a question, and free text is detected", () => {
  const entries = fold([
    {
      type: "extension_ui_request",
      id: "q1",
      method: "select",
      title: "Which branch?",
      options: ["main", "dev", "Other (type your own)"],
    },
  ]);
  const prompt = entries[0];
  if (prompt?.kind !== "prompt") throw new Error("expected a prompt entry");
  expect(prompt.prompt_kind).toBe("question");
  expect(prompt.allow_free_text).toBe(true);
});

test("a cancelled dialog stops asking", () => {
  const entries = fold([
    {
      type: "extension_ui_request",
      id: "p1",
      method: "select",
      title: "Allow tool: bash\nCommand: rm -rf /",
      options: ["Approve", "Deny"],
    },
    { type: "extension_ui_request", id: "c1", method: "cancel", targetId: "p1" },
  ]);
  expect(entries).toHaveLength(1);
  const prompt = entries[0];
  if (prompt?.kind !== "prompt") throw new Error("expected a prompt entry");
  expect(prompt.state).toBe("cancelled");
});

test("unknown frame types are ignored, not errors", () => {
  // omp adds frames across versions; an unrecognised one must cost nothing.
  expect(fold([{ type: "ttsr_triggered", whatever: 1 }, { type: "agent_end" }])).toEqual([]);
});

test("image blocks in a tool result are replaced, not embedded", () => {
  // A base64 screenshot would blow the frame payload cap on its own.
  const entries = fold([
    { type: "tool_execution_start", toolCallId: "t2", toolName: "screenshot", args: {} },
    {
      type: "tool_execution_end",
      toolCallId: "t2",
      toolName: "screenshot",
      result: {
        content: [
          { type: "text", text: "captured " },
          { type: "image", data: "iVBORw0KGgo=", mimeType: "image/png" },
        ],
      },
      isError: false,
    },
  ]);
  const tool = entries[0];
  if (tool?.kind !== "tool") throw new Error("expected a tool entry");
  expect(tool.text).toBe("captured [image]");
});
