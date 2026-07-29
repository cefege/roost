import { expect, test } from "bun:test";
import { asSessionId } from "@roost/shared/wire";
import { AGENT_ENTRY_CAPS } from "@roost/shared/wire/agent-entry";
import type { AgentEntriesFrame } from "@roost/shared/proto/sync_pb";
import { agentEntryFromProto } from "@roost/shared/wire/agent-proto";
import { AgentController } from "../src/agent/agent-controller.ts";
import type { OmpRpcHandle } from "../src/agent/rpc-process.ts";
import type { RpcFrame } from "../src/agent/rpc-frame.ts";

test("durable resume continues at coord's next seq without reseeding history", async () => {
  const sentCommands: RpcFrame[] = [];
  const requestedCommands: RpcFrame[] = [];
  const frames: AgentEntriesFrame[] = [];
  const rpc: OmpRpcHandle = {
    send(command) {
      sentCommands.push(command);
    },
    async request<T>(command: RpcFrame): Promise<T> {
      requestedCommands.push(command);
      return { todoPhases: [], isStreaming: false } as T;
    },
    ready: Promise.resolve(),
    on() {},
    pid: 123,
    kill() {},
    exited: new Promise<number>(() => {}),
    stderrTail: () => "",
  };
  const controller = new AgentController({
    sessionId: asSessionId("11111111-1111-4111-8111-111111111111"),
    rpc,
    nextSeq: 5,
    sendEntries: (frame) => frames.push(frame),
    emitEvent: () => {},
  });

  await controller.start({ resumed: true });
  await controller.abort();
  controller.userMessage("after restart");
  controller.dispose();
  const entries = frames.flatMap((frame) => frame.entries.map(agentEntryFromProto));

  expect(requestedCommands.some((command) => command.type === "abort")).toBe(true);
  expect(requestedCommands.some((command) => command.type === "get_messages_page")).toBe(false);
  expect(sentCommands).toContainEqual({ type: "set_subagent_subscription", level: "progress" });
  expect(entries.map((entry) => entry.seq)).toEqual([5, 6]);
  expect(entries.map((entry) => entry.kind)).toEqual(["notice", "user"]);
  const notice = entries[0];
  if (notice?.kind !== "notice") throw new Error("expected resumed notice");
  expect(notice.text).toBe("resumed");
});

test("oversized todo state is replaced by bounded valid JSON", async () => {
  const frames: AgentEntriesFrame[] = [];
  const rpc: OmpRpcHandle = {
    send() {},
    async request<T>(): Promise<T> {
      return {
        todoPhases: [{
          name: "phase",
          tasks: [{ content: "x".repeat(AGENT_ENTRY_CAPS.text + 1), status: "pending" }],
        }],
        isStreaming: false,
      } as T;
    },
    ready: Promise.resolve(),
    on() {},
    pid: 123,
    kill() {},
    exited: new Promise<number>(() => {}),
    stderrTail: () => "",
  };
  const controller = new AgentController({
    sessionId: asSessionId("22222222-2222-4222-8222-222222222222"),
    rpc,
    sendEntries: (frame) => frames.push(frame),
    emitEvent: () => {},
  });

  await controller.start({ resumed: false });
  controller.dispose();

  const todo = frames
    .flatMap((frame) => frame.entries.map(agentEntryFromProto))
    .find((entry) => entry.kind === "todo");
  if (todo?.kind !== "todo") throw new Error("expected bounded todo entry");
  expect(todo.phases_json.length).toBeLessThanOrEqual(AGENT_ENTRY_CAPS.text);
  expect(() => JSON.parse(todo.phases_json)).not.toThrow();
  expect(todo.phases_json).toContain("Todo state exceeded");
});
