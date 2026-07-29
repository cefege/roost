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
  type ProjectionOp,
} from "../src/agent/entry-projection.ts";
import { projectSessionMessage } from "../src/agent/history-projection.ts";

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

function materializeProjectionOps(ops: ProjectionOp[]): AgentEntry[] {
  const bySeq = new Map<number, AgentEntry>();
  const order: number[] = [];
  for (const op of ops) {
    if (op.op === "append") {
      bySeq.set(op.entry.seq, op.entry);
      order.push(op.entry.seq);
      continue;
    }
    const entry = bySeq.get(op.seq);
    if (entry) applyEntryPatch(entry, op.patch);
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

test("omp notices preserve their level and identify their source", () => {
  const entries = fold([
    { type: "notice", level: "info", message: "xd://: mounted x", source: "xdev" },
  ]);
  expect(entries).toHaveLength(1);
  const notice = entries[0];
  if (notice?.kind !== "notice") throw new Error("expected a notice entry");
  expect(notice.level).toBe("info");
  expect(notice.text).toBe("xdev: xd://: mounted x");
});

test("source warning notices remain warnings", () => {
  const entries = fold([
    { type: "notice", level: "warning", message: "quota nearly exhausted", source: "provider" },
  ]);
  const notice = entries[0];
  if (notice?.kind !== "notice") throw new Error("expected a notice entry");
  expect(notice.level).toBe("warn");
  expect(notice.text).toBe("provider: quota nearly exhausted");
});

test("an assistant message_end exposes provider errors", () => {
  const entries = fold([
    {
      type: "message_end",
      message: {
        role: "assistant",
        stopReason: "error",
        errorMessage: "Anthropic stream error (overloaded_error): Overloaded",
      },
    },
  ]);
  expect(entries).toHaveLength(1);
  const notice = entries[0];
  if (notice?.kind !== "notice") throw new Error("expected a notice entry");
  expect(notice.level).toBe("error");
  expect(notice.text).toContain("overloaded_error");
});

test("assistant abort sentinels are not rendered as provider errors", () => {
  const entries = fold([
    { type: "message_end", message: { role: "assistant", stopReason: "aborted", errorMessage: "__omp.silent_abort__" } },
    { type: "message_end", message: { role: "assistant", stopReason: "aborted", errorMessage: "Interrupted by user" } },
    { type: "message_end", message: { role: "assistant", stopReason: "aborted", errorMessage: "Request was aborted" } },
  ]);
  expect(entries).toHaveLength(1);
  const notice = entries[0];
  if (notice?.kind !== "notice") throw new Error("expected a neutral abort notice");
  expect(notice.level).toBe("info");
  expect(notice.text).toBe("Operation aborted");
});

test("auto_retry_start exposes the retry attempt as a warning", () => {
  const entries = fold([
    {
      type: "auto_retry_start",
      attempt: 1,
      maxAttempts: 10,
      errorMessage: "Anthropic stream error (overloaded_error): Overloaded",
    },
  ]);
  expect(entries).toHaveLength(1);
  const notice = entries[0];
  if (notice?.kind !== "notice") throw new Error("expected a notice entry");
  expect(notice.level).toBe("warn");
  expect(notice.text).toContain("1/10");
});

test("retry fallback and terminal failure preserve their visible severity", () => {
  const entries = fold([
    { type: "retry_fallback_applied", from: "fast", to: "base", role: "fireworks-fast" },
    { type: "retry_fallback_succeeded", model: "base", role: "fireworks-fast" },
    { type: "auto_retry_end", success: false, attempt: 3, finalError: "Overloaded" },
  ]);
  expect(entries.map((entry) => entry.kind === "notice" ? entry.level : "")).toEqual([
    "warn",
    "info",
    "error",
  ]);
  expect(entries.map((entry) => entry.kind === "notice" ? entry.text : "")).toEqual([
    "Fallback: fast -> base",
    "Fallback succeeded on base",
    "Retry failed after 3 attempts: Overloaded",
  ]);
});

test("custom message_end content is attributed to its custom type", () => {
  const entries = fold([
    {
      type: "message_end",
      message: {
        role: "custom",
        customType: "xdev-mount-notice",
        content: "<system-notice>mounted</system-notice>",
        display: true,
        timestamp: TS,
      },
    },
  ]);
  expect(entries).toHaveLength(1);
  const notice = entries[0];
  if (notice?.kind !== "notice") throw new Error("expected a notice entry");
  expect(notice.level).toBe("info");
  expect(notice.text).toMatch(/^xdev-mount-notice: /);
});

test("hidden custom scaffolding does not leak into the transcript", () => {
  const entries = fold([
    {
      type: "message_end",
      message: {
        role: "custom",
        customType: "eager-todo-prelude",
        content: "internal model instructions",
        display: false,
      },
    },
  ]);
  expect(entries).toEqual([]);
});

test("source-shaped subagent snapshots replace progress through completion", () => {
  const progress = (recentOutput: string[]) => ({
    index: 0,
    id: "sub-1",
    agent: "scout",
    agentSource: "bundled",
    status: "running",
    task: "Check docs",
    recentTools: [],
    recentOutput,
    toolCount: 0,
    requests: 0,
    tokens: 0,
    cost: 0,
    durationMs: 1,
  });
  const entries = fold([
    {
      type: "subagent_lifecycle",
      payload: {
        id: "sub-1",
        agent: "scout",
        agentSource: "bundled",
        description: "Researcher",
        status: "started",
        index: 0,
      },
    },
    {
      type: "subagent_progress",
      payload: {
        index: 0,
        agent: "scout",
        agentSource: "bundled",
        task: "Check docs",
        progress: progress(["checking docs"]),
      },
    },
    {
      type: "subagent_progress",
      payload: {
        index: 0,
        agent: "scout",
        agentSource: "bundled",
        task: "Check docs",
        progress: progress(["found answer", "checking docs"]),
      },
    },
    {
      type: "subagent_lifecycle",
      payload: {
        id: "sub-1",
        agent: "scout",
        agentSource: "bundled",
        description: "Researcher",
        status: "completed",
        index: 0,
      },
    },
  ]);
  expect(entries).toHaveLength(1);
  expect(entries.map((entry) => entry.seq)).toEqual([1]);
  const subagent = entries[0];
  if (subagent?.kind !== "subagent") throw new Error("expected a subagent entry");
  expect(subagent.subagent_id).toBe("sub-1");
  expect(subagent.name).toBe("Researcher");
  expect(subagent.state).toBe("done");
  expect(subagent.text).toBe("checking docs\nfound answer");
});

test("an aborted subagent remains distinct from a failed subagent", () => {
  const entries = fold([{
    type: "subagent_lifecycle",
    payload: {
      id: "sub-abort",
      agent: "scout",
      agentSource: "bundled",
      status: "aborted",
      index: 0,
    },
  }]);
  const subagent = entries[0];
  if (subagent?.kind !== "subagent") throw new Error("expected a subagent entry");
  expect(subagent.state).toBe("aborted");
});

test("ttsr_triggered exposes the injected rule panel content", () => {
  const entries = fold([{
    type: "ttsr_triggered",
    rules: [{ name: "No guessing", description: "Read the source first." }],
  }]);
  expect(entries).toHaveLength(1);
  const notice = entries[0];
  if (notice?.kind !== "notice") throw new Error("expected a notice entry");
  expect(notice.level).toBe("warn");
  expect(notice.text).toBe("Injecting rule\nNo guessing: Read the source first.");
});

test("repaint-only frames do not enter the transcript", () => {
  // setWidget arrives as the same frame type as a real approval but carries no
  // title/options and needs no reply. The other frames are lifecycle/chrome
  // repaint chatter rather than durable transcript content.
  const entries = fold([
    { type: "extension_ui_request", id: "1542eb00", method: "setWidget", widgetKey: "autoresearch" },
    { type: "available_commands_update", commands: [] },
    { type: "turn_start" },
  ]);
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

test("unknown and agent_end frames are ignored, not errors", () => {
  // omp adds frames across versions; an unrecognised one must cost nothing.
  expect(fold([{ type: "future_frame", whatever: 1 }, { type: "agent_end" }])).toEqual([]);
});

test("image blocks in a tool result become separate image entries", () => {
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
  expect(entries.map((entry) => entry.kind)).toEqual(["tool", "image"]);

  const tool = entries[0];
  if (tool?.kind !== "tool") throw new Error("expected a tool entry");
  expect(tool.text).toBe("captured ");
  expect(tool.text).not.toContain("[image]");

  const image = entries[1];
  if (image?.kind !== "image") throw new Error("expected an image entry");
  expect(image.media_type).toBe("image/png");
  expect(image.data_b64).toBe("iVBORw0KGgo=");
  expect(image.alt).toBe("screenshot result image");
});

test("history projection preserves assistant and tool-result images", () => {
  const assistantState = newProjectionState();
  const assistantOps = projectSessionMessage(
    {
      role: "assistant",
      content: [
        { type: "text", text: "before image" },
        { type: "image", data: "aGlzdG9yeQ==", mimeType: "image/png" },
        { type: "text", text: "after image" },
      ],
    },
    assistantState,
    TS,
  );
  const assistantEntries = materializeProjectionOps(assistantOps);

  expect(assistantEntries.map((entry) => entry.kind)).toEqual([
    "assistant",
    "image",
    "assistant",
  ]);
  expect(assistantEntries.map((entry) => entry.seq)).toEqual([1, 2, 3]);
  const assistantImage = assistantEntries[1];
  if (assistantImage?.kind !== "image") throw new Error("expected an image entry");
  expect(assistantImage.media_type).toBe("image/png");
  expect(assistantImage.data_b64).toBe("aGlzdG9yeQ==");

  const toolState = newProjectionState();
  const toolCallOps = projectSessionMessage(
    {
      role: "assistant",
      content: [
        {
          type: "toolCall",
          id: "history-tool-1",
          name: "screenshot",
          arguments: {},
        },
      ],
    },
    toolState,
    TS,
  );
  const toolResultOps = projectSessionMessage(
    {
      role: "toolResult",
      toolCallId: "history-tool-1",
      content: [
        { type: "text", text: "captured " },
        { type: "image", data: "aGlzdG9yeS10b29s", mimeType: "image/png" },
      ],
      isError: false,
    },
    toolState,
    TS,
  );

  expect(toolResultOps.map((op) => op.op)).toEqual(["update", "append"]);
  const toolEntries = materializeProjectionOps([...toolCallOps, ...toolResultOps]);
  expect(toolEntries.map((entry) => entry.kind)).toEqual(["tool", "image"]);
  const tool = toolEntries[0];
  if (tool?.kind !== "tool") throw new Error("expected a tool entry");
  expect(tool.text).toBe("captured ");
  expect(tool.text).not.toContain("[image]");

  const toolImage = toolEntries[1];
  if (toolImage?.kind !== "image") throw new Error("expected an image entry");
  expect(toolImage.media_type).toBe("image/png");
  expect(toolImage.data_b64).toBe("aGlzdG9yeS10b29s");
});


test("history projection respects OMP visibility and preserves seq order", () => {
  const state = newProjectionState();
  expect(projectSessionMessage({
    role: "custom",
    customType: "eager-task-prelude",
    content: "internal",
    display: false,
  }, state, TS)).toEqual([]);

  const execution = materializeProjectionOps(projectSessionMessage({
    role: "bashExecution",
    command: "printf ok",
    output: "ok",
    exitCode: 0,
    cancelled: false,
  }, state, TS));
  expect(execution[0]?.kind).toBe("notice");

  const files = materializeProjectionOps(projectSessionMessage({
    role: "fileMention",
    files: [
      { path: "a.ts", content: "a" },
      {
        path: "diagram.png",
        content: "",
        image: { type: "image", data: "aW1hZ2U=", mimeType: "image/png" },
      },
    ],
  }, state, TS));
  expect(files.map((entry) => entry.kind)).toEqual(["notice", "image"]);
  expect(files.map((entry) => entry.seq)).toEqual(
    [...files.map((entry) => entry.seq)].sort((a, b) => a - b),
  );
});

test("history projection suppresses user interrupts but retains provider failures", () => {
  const state = newProjectionState();
  const interrupted = projectSessionMessage({
    role: "assistant",
    content: [],
    errorMessage: "Interrupted by user",
  }, state, TS);
  expect(interrupted).toEqual([]);

  const failed = materializeProjectionOps(projectSessionMessage({
    role: "assistant",
    content: [],
    stopReason: "error",
    errorMessage: "Anthropic stream error (overloaded_error): Overloaded",
  }, state, TS));
  const notice = failed[0];
  if (notice?.kind !== "notice") throw new Error("expected provider failure notice");
  expect(notice.level).toBe("error");
  expect(notice.text).toContain("overloaded_error");
});

test("assistant image events use content and interrupted streams are finalized", () => {
  const entries = fold([
    textFrame({ type: "text_start", contentIndex: 0 }),
    textFrame({ type: "text_delta", contentIndex: 0, delta: "before image" }),
    textFrame({
      type: "image_end",
      contentIndex: 1,
      content: { type: "image", data: "aW1hZ2U=", mimeType: "image/png" },
    }),
    {
      type: "message_end",
      message: {
        role: "assistant",
        stopReason: "aborted",
        errorId: 0x0400_0000,
      },
    },
  ]);

  expect(entries.map((entry) => entry.kind)).toEqual(["assistant", "image"]);
  const assistant = entries[0];
  if (assistant?.kind !== "assistant") throw new Error("expected assistant entry");
  expect(assistant.text).toBe("before image");
  expect(assistant.done).toBe(true);
  const image = entries[1];
  if (image?.kind !== "image") throw new Error("expected image entry");
  expect(image.data_b64).toBe("aW1hZ2U=");
  expect(image.media_type).toBe("image/png");
});

test("background tools remain live until a terminal update and retain details", () => {
  const entries = fold([
    { type: "tool_execution_start", toolCallId: "bg-1", toolName: "task", args: {} },
    {
      type: "tool_execution_update",
      toolCallId: "bg-1",
      partialResult: {
        content: [{ type: "text", text: "starting" }],
        details: { progress: [{ id: "child", status: "running" }] },
      },
    },
    {
      type: "tool_execution_end",
      toolCallId: "bg-1",
      toolName: "task",
      result: {
        content: [{ type: "text", text: "running in background" }],
        details: { async: { state: "running", jobId: "job-1" } },
      },
      isError: false,
    },
    {
      type: "tool_execution_update",
      toolCallId: "bg-1",
      partialResult: {
        content: [{ type: "text", text: "finished" }],
        details: { async: { state: "completed", jobId: "job-1" }, result: "ok" },
      },
    },
    {
      type: "tool_execution_update",
      toolCallId: "bg-1",
      partialResult: {
        content: [{ type: "text", text: "must be ignored" }],
        details: { async: { state: "failed", jobId: "job-1" } },
      },
    },
  ]);

  const tool = entries[0];
  if (tool?.kind !== "tool") throw new Error("expected tool entry");
  expect(tool.status).toBe("ok");
  expect(tool.text).toBe("finished");
  expect(JSON.parse(tool.details_json)).toEqual({
    async: { state: "completed", jobId: "job-1" },
    result: "ok",
  });
});

test("IRC twins dedupe and visible custom details remain structured", () => {
  const message = {
    role: "custom",
    customType: "irc",
    timestamp: TS,
    display: true,
    content: "peer update",
    details: { from: "Researcher", channel: "task" },
  };
  const entries = fold([
    { type: "irc_message", message },
    { type: "message_end", message },
    {
      type: "message_end",
      message: {
        role: "custom",
        customType: "hidden-without-display",
        timestamp: TS + 1,
        content: "do not render",
      },
    },
  ]);

  expect(entries).toHaveLength(1);
  const notice = entries[0];
  if (notice?.kind !== "notice") throw new Error("expected notice entry");
  expect(notice.text).toBe("irc: peer update");
  expect(JSON.parse(notice.details_json)).toEqual({
    from: "Researcher",
    channel: "task",
  });
});

test("structured abort flags and stop reasons control terminal severity", () => {
  const entries = fold([
    {
      type: "message_end",
      message: { role: "assistant", stopReason: "aborted", errorId: 0x0200_0000 },
    },
    {
      type: "message_end",
      message: { role: "assistant", stopReason: "aborted", errorId: 0x0400_0000 },
    },
    {
      type: "message_end",
      message: { role: "assistant", stopReason: "aborted", errorId: 0x0800_0000 },
    },
    {
      type: "message_end",
      message: { role: "assistant", errorMessage: "not terminal" },
    },
    {
      type: "message_end",
      message: { role: "assistant", stopReason: "error", errorMessage: "provider failed" },
    },
  ]);

  expect(entries.map((entry) => entry.kind === "notice" ? [entry.level, entry.text] : [])).toEqual([
    ["info", "Operation aborted"],
    ["error", "provider failed"],
  ]);
});

test("live non-assistant message roles preserve visible source order", () => {
  const entries = fold([
    {
      type: "message_end",
      message: { role: "user", content: [{ type: "text", text: "local echo duplicate" }] },
    },
    {
      type: "message_end",
      message: {
        role: "user",
        synthetic: true,
        content: [{ type: "text", text: "synthetic input" }],
      },
    },
    {
      type: "message_end",
      message: { role: "developer", content: [{ type: "text", text: "developer input" }] },
    },
    {
      type: "message_end",
      message: { role: "bashExecution", command: "echo ok", output: "ok", exitCode: 0 },
    },
    {
      type: "message_end",
      message: { role: "pythonExecution", code: "print(1)", output: "1", exitCode: 0 },
    },
    {
      type: "message_end",
      message: { role: "branchSummary", summary: "branched", warning: "" },
    },
    {
      type: "message_end",
      message: {
        role: "fileMention",
        files: [{
          path: "diagram.png",
          image: { type: "image", data: "ZmlsZQ==", mimeType: "image/png" },
        }],
      },
    },
  ]);

  expect(entries.map((entry) => entry.kind)).toEqual([
    "user",
    "user",
    "notice",
    "notice",
    "notice",
    "notice",
    "image",
  ]);
  expect(entries.filter((entry) => entry.kind === "user").map((entry) => entry.text)).toEqual([
    "synthetic input",
    "developer input",
  ]);
  expect(entries.map((entry) => entry.seq)).toEqual([1, 2, 3, 4, 5, 6, 7]);
});

test("todo, maintenance, URL, TTSR and subagent retry surfaces stay visible", () => {
  const entries = fold([
    {
      type: "todo_reminder",
      todos: [{ content: "Finish parity", status: "in_progress" }],
      attempt: 2,
      maxAttempts: 3,
    },
    { type: "auto_compaction_start", reason: "overflow", action: "handoff" },
    {
      type: "extension_ui_request",
      id: "url-1",
      method: "open_url",
      url: "https://provider.example/very-long",
      launchUrl: "http://127.0.0.1:9911",
      instructions: "Open this URL",
    },
    { type: "ttsr_triggered", rules: [] },
    {
      type: "subagent_lifecycle",
      payload: {
        id: "retry-child",
        agent: "scout",
        status: "started",
        index: 0,
      },
    },
    {
      type: "subagent_progress",
      payload: {
        index: 0,
        agent: "scout",
        progress: {
          id: "retry-child",
          agent: "scout",
          status: "running",
          recentOutput: [],
          retryState: { attempt: 2, maxAttempts: 4, errorMessage: "rate limited" },
        },
      },
    },
  ]);

  const todo = entries[0];
  if (todo?.kind !== "todo") throw new Error("expected todo entry");
  expect(JSON.parse(todo.phases_json)[0].name).toBe("Todo reminder 2/3");
  expect(entries[1]?.kind === "notice" ? entries[1].text : "").toBe(
    "Context overflow detected, Auto-handoff…",
  );
  expect(entries[2]?.kind === "notice" ? entries[2].text : "").toContain(
    "http://127.0.0.1:9911",
  );
  expect(entries[3]?.kind === "notice" ? entries[3].text : "").toBe(
    "tool-time system reminder injected",
  );
  expect(entries[4]?.kind === "subagent" ? entries[4].text : "").toBe(
    "retrying 2/4: rate limited",
  );
});

test("history retains ordinary user and developer messages during reconstruction", () => {
  const state = newProjectionState();
  const entries = materializeProjectionOps([
    ...projectSessionMessage(
      { role: "user", content: [{ type: "text", text: "original prompt" }] },
      state,
      TS,
    ),
    ...projectSessionMessage(
      { role: "developer", content: [{ type: "text", text: "injected guidance" }] },
      state,
      TS + 1,
    ),
  ]);

  expect(entries.map((entry) => entry.kind)).toEqual(["user", "user"]);
  expect(entries.map((entry) => entry.kind === "user" ? entry.text : "")).toEqual([
    "original prompt",
    "injected guidance",
  ]);
});
