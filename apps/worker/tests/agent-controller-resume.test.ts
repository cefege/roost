import { expect, test } from "bun:test";
import { asSessionId } from "@roost/shared/wire";
import { AGENT_ENTRY_CAPS } from "@roost/shared/wire/agent-entry";
import type { AgentEntriesFrame, AgentUiFrame } from "@roost/shared/proto/sync_pb";
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
    sendUiFrame: () => {},
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
    sendUiFrame: () => {},
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

test("canonical OMP UI frames preserve order and snapshot boundary", async () => {
  const sentCommands: RpcFrame[] = [];
  const uiFrames: AgentUiFrame[] = [];
  const legacyFrames: AgentEntriesFrame[] = [];
  let onFrame: ((frame: RpcFrame) => void) | undefined;
  const rpc: OmpRpcHandle = {
    send(command) {
      sentCommands.push(command);
    },
    async request<T>(): Promise<T> {
      return { todoPhases: [], isStreaming: false } as T;
    },
    ready: Promise.resolve(),
    on(listener) {
      onFrame = listener;
    },
    pid: 123,
    kill() {},
    exited: new Promise<number>(() => {}),
    stderrTail: () => "",
  };
  const controller = new AgentController({
    sessionId: asSessionId("33333333-3333-4333-8333-333333333333"),
    rpc,
    sendEntries: (frame) => legacyFrames.push(frame),
    sendUiFrame: (frame) => uiFrames.push(frame),
    emitEvent: () => {},
  });

  await controller.start({ resumed: false });
  expect(sentCommands[0]).toEqual({ type: "subscribe_ui" });

  const hostFrames: RpcFrame[] = [
    { t: "welcome", proto: 2, entryCount: 1 },
    { t: "snapshot-chunk", entries: [{ id: "entry-1" }], final: false },
    { t: "snapshot-chunk", entries: [], final: true },
    { t: "state", state: { isStreaming: false } },
  ];
  if (!onFrame) throw new Error("RPC listener was not installed");
  for (const frame of hostFrames) onFrame({ type: "ui_frame", frame });

  expect(uiFrames.map((frame) => frame.frameJson)).toEqual(
    hostFrames.map((frame) => JSON.stringify(frame)),
  );
  const snapshotId = uiFrames[0]?.snapshotId;
  expect(snapshotId).toBeTruthy();
  expect(uiFrames.slice(0, 3).map((frame) => frame.snapshotId)).toEqual([
    snapshotId,
    snapshotId,
    snapshotId,
  ]);
  expect(uiFrames[3]?.snapshotId).toBe("");

  controller.subscribeUi();
  const interruptedStart = uiFrames.length;
  onFrame({ type: "ui_frame", frame: { t: "welcome", proto: 2, entryCount: 1 } });
  onFrame({
    type: "ui_frame",
    frame: { t: "snapshot-chunk", entries: [{ id: "lost-entry" }], final: false },
  });
  const interruptedFrames = uiFrames.slice(interruptedStart);
  const interruptedId = interruptedFrames[0]?.snapshotId;
  expect(interruptedId).toBeTruthy();
  expect(interruptedFrames.map((frame) => frame.snapshotId)).toEqual([
    interruptedId,
    interruptedId,
  ]);

  controller.subscribeUi();
  const recoveredStart = uiFrames.length;
  onFrame({ type: "ui_frame", frame: { t: "welcome", proto: 2, entryCount: 0 } });
  onFrame({ type: "ui_frame", frame: { t: "snapshot-chunk", entries: [], final: true } });
  onFrame({ type: "ui_frame", frame: { t: "state", state: { isStreaming: false } } });
  const recoveredFrames = uiFrames.slice(recoveredStart);
  const recoveredId = recoveredFrames[0]?.snapshotId;
  expect(recoveredId).toBeTruthy();
  expect(recoveredId).not.toBe(interruptedId);
  expect(recoveredFrames.map((frame) => frame.snapshotId)).toEqual([
    recoveredId,
    recoveredId,
    "",
  ]);
  expect(sentCommands.filter((command) => command.type === "subscribe_ui")).toHaveLength(3);
  expect(uiFrames.every((frame) => frame.coordRevision === 0n)).toBe(true);
  expect(legacyFrames).toHaveLength(0);
  controller.dispose();
});

test("reconnect before OMP ready does not duplicate the initial UI subscription", async () => {
  const ready = Promise.withResolvers<void>();
  const sentCommands: RpcFrame[] = [];
  const rpc: OmpRpcHandle = {
    send(command) {
      sentCommands.push(command);
    },
    async request<T>(): Promise<T> {
      return { todoPhases: [], isStreaming: false } as T;
    },
    ready: ready.promise,
    on() {},
    pid: 123,
    kill() {},
    exited: new Promise<number>(() => {}),
    stderrTail: () => "",
  };
  const controller = new AgentController({
    sessionId: asSessionId("44444444-4444-4444-8444-444444444444"),
    rpc,
    sendEntries: () => {},
    sendUiFrame: () => {},
    emitEvent: () => {},
  });

  const starting = controller.start({ resumed: false });
  controller.subscribeUi();
  expect(sentCommands.filter((command) => command.type === "subscribe_ui")).toHaveLength(0);
  ready.resolve();
  await starting;
  expect(sentCommands.filter((command) => command.type === "subscribe_ui")).toHaveLength(1);
  controller.dispose();
});
