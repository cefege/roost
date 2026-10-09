import type { AgentEvent, SnapshotEvent } from "@earendil-works/pi-durable";
import { expect, test } from "vitest";
import { ChatTranslator } from "../src/translate.ts";

function translate(translator: ChatTranslator, event: Record<string, unknown>) {
  return translator.translate(event as AgentEvent);
}

test("snapshot reset resolves tool arguments from assistant tool calls", () => {
  const translator = new ChatTranslator();
  const snapshot = {
    type: "snapshot",
    entries: [
      { id: 11, model: [{ role: "assistant", content: [{ type: "toolCall", id: "call-1", name: "bash", arguments: { command: "ls -la" } }] }] },
      { id: 12, model: [{ role: "toolResult", toolCallId: "call-1", toolName: "bash", content: [{ type: "text", text: "file.txt" }], isError: false }] },
    ],
    run: undefined,
    generation: undefined,
    tools: [{ callId: "call-2", name: "read", status: "running", output: "partial" }],
    compactions: [],
    inbox: [],
    agent: { model: { provider: "faux", modelId: "fake" }, thinkingLevel: "off" },
    usage: { models: { "faux/fake": { input: 3, output: 2, cost: { total: 0.01 } } }, tools: {} },
  } as unknown as SnapshotEvent;
  const [reset] = translator.snapshot(snapshot);
  expect(reset?.type).toBe("reset");
  expect(reset?.transcript.items[1]).toMatchObject({ kind: "tool", call_id: "call-1", args_json: JSON.stringify({ command: "ls -la" }), output: "file.txt" });
  expect(reset?.transcript.items[2]).toMatchObject({ kind: "tool", call_id: "call-2", args_json: "{}", running: true });
  expect(reset?.transcript).toMatchObject({ model: { provider: "faux", model_id: "fake" }, usage: { input_tokens: 3, output_tokens: 2, cost_usd: 0.01 } });
});

test("translates assistant start, deltas, full message updates, and completion", () => {
  const translator = new ChatTranslator();
  const start = translate(translator, { type: "message_start", message: { role: "assistant", content: [{ type: "text", text: "Hello" }] } });
  expect(start).toEqual([
    { type: "item", item: { id: "live-1", kind: "assistant", blocks: [{ type: "text", text: "" }], streaming: true, error: null } },
    { type: "text_delta", item_id: "live-1", block: 0, delta: "Hello" },
  ]);
  expect(translate(translator, { type: "message_update", usage: {}, changes: [{ type: "text_delta", contentIndex: 0, delta: " world" }] })).toEqual([{ type: "text_delta", item_id: "live-1", block: 0, delta: " world" }]);
  expect(translate(translator, { type: "message_update", usage: {}, changes: [{ type: "message", message: { role: "assistant", content: [{ type: "text", text: "Hello world!" }] } }] })).toEqual([{ type: "item", item: { id: "live-1", kind: "assistant", blocks: [{ type: "text", text: "Hello world!" }], streaming: true, error: null } }]);
  expect(translate(translator, { type: "message_end", entry: { id: 15, model: [{ role: "assistant", content: [{ type: "text", text: "Hello world!" }] }] } })).toEqual([{ type: "item", item: { id: "live-1", kind: "assistant", blocks: [{ type: "text", text: "Hello world!" }], streaming: false, error: null } }]);
});

test("translates tool execution output and preserves live args", () => {
  const translator = new ChatTranslator();
  expect(translate(translator, { type: "tool_execution_start", toolCallId: "call-2", toolName: "read", args: { path: "src/main.ts" } })[0]?.item).toMatchObject({ args_json: JSON.stringify({ path: "src/main.ts" }), running: true });
  expect(translate(translator, { type: "tool_execution_update", toolCallId: "call-2", toolName: "read", output: { set: "first\nsecond" } })).toEqual([{ type: "tool_output", item_id: "tool-call-2", set: "first\nsecond" }]);
  expect(translate(translator, { type: "tool_execution_update", toolCallId: "call-2", toolName: "read", output: { trimStart: 6, append: "third" } })).toEqual([{ type: "tool_output", item_id: "tool-call-2", trim_start: 6, append: "third" }]);
  expect(translate(translator, { type: "tool_execution_end", toolCallId: "call-2", toolName: "read", entry: { id: 16, model: [{ role: "toolResult", toolCallId: "call-2", toolName: "read", content: [{ type: "text", text: "result" }], isError: false }] } })[0]?.item).toMatchObject({ args_json: JSON.stringify({ path: "src/main.ts" }), output: "result", running: false });
});

test("translates run state, failures, agent changes, and usage", () => {
  const translator = new ChatTranslator();
  expect(translate(translator, { type: "run_start", inputs: [] })).toEqual([{ type: "run_state", run_state: "running", error: null }]);
  expect(translate(translator, { type: "task_failed", taskId: 1, kind: "generation", message: "provider failed" })).toEqual([{ type: "run_state", run_state: "failed", error: "provider failed" }]);
  expect(translator.error).toBe("provider failed");
  expect(translate(translator, { type: "agent_changed", agent: { model: { provider: "faux", modelId: "model-2" }, thinkingLevel: "high" } })).toEqual([{ type: "agent", model: { provider: "faux", model_id: "model-2" }, thinking_level: "high" }]);
  expect(translate(translator, { type: "usage_changed", usage: { models: { "faux/model-2": { input: 8, output: 5, cost: { total: 0.02 } } }, tools: {} } })).toEqual([{ type: "usage", usage: { input_tokens: 8, output_tokens: 5, cost_usd: 0.02 } }]);
  expect(translate(translator, { type: "run_end", inputs: [] })).toEqual([{ type: "run_state", run_state: "idle", error: null }]);
});
