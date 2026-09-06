// Worker transport tests prove the dedicated DAgentPrompt dispatch reaches the
// guarded controller and returns WInputResult truth. Failure coverage pins the
// secrecy boundary: arbitrary dependency errors never enter logs or replies.

import { create } from "@bufbuild/protobuf";
import { afterEach, expect, test } from "bun:test";
import {
  CoordWorkerDownSchema,
  DAgentPromptSchema,
  TerminalInputStatus,
  TerminalWritePhase,
} from "@roost/shared/proto/worker_transport_pb";
import { asWorkerFp } from "@roost/shared/wire";
import { AgentStatusRegistry } from "../src/agent-status/registry.ts";
import type { CoordTarget } from "../src/coord-target.ts";
import {
  buildCoordLinkDeps,
  type CoordLinkRefs,
} from "../src/coord-link-deps.ts";
import type { WorkerCoordRelocation } from "../src/coord-relocation.ts";
import { MuxFrameType } from "../src/keeper/protocol.ts";
import { createCoordLinkDownstream } from "../src/transport/coord-link-downstream.ts";
import type {
  CoordLink,
  CoordLinkDeps,
  CoordLinkOutbox,
  UpstreamFrame,
} from "../src/transport/coord-link-types.ts";
import type { SessionEventStore } from "../src/transport/session-event-store.ts";
import { installFakeKeeper, type FakeKeeper } from "./keeper-fake-pool.ts";
import {
  cleanupStreamHarnesses,
  makeHarness,
  SESSION_ID,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";

const PROCESS_ID = 9_001;
const arbitraryErrorSecret = "prompt-and-status-secret-from-dependency";
const registries: AgentStatusRegistry[] = [];

afterEach(() => {
  for (const registry of registries.splice(0)) registry.dispose();
  cleanupStreamHarnesses();
});

test("agentPrompt dispatch writes once and returns an accepted WInputResult", async () => {
  const stream = await makeHarness();
  stream.record.childPid = 100;
  const registry = new AgentStatusRegistry({
    publish: () => {},
    now: () => 1_000,
    leaseMs: 60_000,
    startLeaseTimer: false,
  });
  registries.push(registry);
  registry.reportIntegration({
    sessionId: String(SESSION_ID),
    agentId: "omp",
    processId: PROCESS_ID,
    state: "working",
    seq: 1,
    active: true,
  });
  const proof = registry.currentPrivateProof(String(SESSION_ID));
  if (!proof) throw new Error("transport prompt proof was not created");
  const detector = {
    reportingAgentForSession: async (sessionId: string, processId: number) => (
      sessionId === String(SESSION_ID) && processId === PROCESS_ID
        ? { agentId: "omp" as const, pid: PROCESS_ID }
        : null
    ),
  };

  let keeper!: FakeKeeper;
  keeper = trackKeeper(installFakeKeeper({
    onWrite: (write) => {
      if (write.type === MuxFrameType.PtyInRequest) {
        keeper.inputAck(write.channelId, write.seq!, write.bytes!.byteLength);
      }
    },
  }));
  const reply = Promise.withResolvers<UpstreamFrame>();
  const link = {
    send: (frame: UpstreamFrame) => {
      if (frame.kind === "input-result") reply.resolve(frame);
      return true;
    },
  } as unknown as CoordLink;
  const refs: CoordLinkRefs = {
    link,
    sessionMgr: stream.manager,
    agentRegistry: registry,
    agentDetector: detector,
    acquireKeeperUpdateBoundary: null,
  };
  const deps = buildCoordLinkDeps({
    coordHttpUrl: "https://coord.test",
    workerFp: asWorkerFp("00".repeat(32)),
    mintJwt: async () => "jwt",
    sessionEventStore: {} as unknown as SessionEventStore,
    coordTarget: {} as unknown as CoordTarget,
    relocation: {} as unknown as WorkerCoordRelocation,
    setCoordinatorEndpoint: () => {},
    refs,
  });
  const socket = {} as WebSocket;
  const downstream = createCoordLinkDownstream(deps, {
    send: () => true,
    activeSocket: () => socket,
  } as unknown as CoordLinkOutbox);
  const request = create(DAgentPromptSchema, {
    requestId: "transport-agent-prompt",
    sessionId: String(SESSION_ID),
    inputSeq: 7n,
    expectedStatusEpoch: proof.statusEpoch,
    expectedOccupantId: proof.occupantId,
    expectedRevision: BigInt(proof.revision),
    text: "go",
    budgetMs: 5_000,
  });
  downstream.handleDownstream(create(CoordWorkerDownSchema, {
    frame: { case: "agentPrompt", value: request },
  }), false, socket);

  expect(await reply.promise).toEqual({
    kind: "input-result",
    request_id: "transport-agent-prompt",
    session_id: String(SESSION_ID),
    input_seq: 7n,
    status: TerminalInputStatus.ACCEPTED,
    written_bytes: 3,
    phase: TerminalWritePhase.WRITTEN,
    reason: undefined,
  });
  expect(keeper.writes.filter((write) => write.type === MuxFrameType.PtyInRequest))
    .toHaveLength(1);
});

test("agentPrompt dependency failures use static ambiguous output without logging secrets", async () => {
  const request = create(DAgentPromptSchema, {
    requestId: "failing-agent-prompt",
    sessionId: String(SESSION_ID),
    inputSeq: 8n,
    expectedStatusEpoch: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    expectedOccupantId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    expectedRevision: 1n,
    text: arbitraryErrorSecret,
    budgetMs: 5_000,
  });
  const sent = Promise.withResolvers<UpstreamFrame>();
  const socket = {} as WebSocket;
  const downstream = createCoordLinkDownstream({
    onAgentPrompt: async () => {
      throw new Error(arbitraryErrorSecret);
    },
  } as unknown as CoordLinkDeps, {
    send: (frame: UpstreamFrame) => {
      sent.resolve(frame);
      return true;
    },
    activeSocket: () => socket,
  } as unknown as CoordLinkOutbox);
  const logs: string[] = [];
  const originalError = console.error;
  console.error = (...values: unknown[]) => { logs.push(values.map(String).join(" ")); };
  try {
    downstream.handleDownstream(create(CoordWorkerDownSchema, {
      frame: { case: "agentPrompt", value: request },
    }), false, socket);
    expect(await sent.promise).toEqual({
      kind: "input-result",
      request_id: "failing-agent-prompt",
      session_id: String(SESSION_ID),
      input_seq: 8n,
      status: TerminalInputStatus.AMBIGUOUS,
      written_bytes: 0,
      phase: TerminalWritePhase.UNKNOWN,
      reason: "worker agent prompt handler failed",
    });
  } finally {
    console.error = originalError;
  }
  expect(logs.join("\n")).not.toContain(arbitraryErrorSecret);
});

test("agentPrompt without a bound worker handler rejects synchronously", () => {
  const sent: UpstreamFrame[] = [];
  const socket = {} as WebSocket;
  const downstream = createCoordLinkDownstream({} as unknown as CoordLinkDeps, {
    send: (frame: UpstreamFrame) => {
      sent.push(frame);
      return true;
    },
    activeSocket: () => socket,
  } as unknown as CoordLinkOutbox);
  downstream.handleDownstream(create(CoordWorkerDownSchema, {
    frame: {
      case: "agentPrompt",
      value: create(DAgentPromptSchema, {
        requestId: "unbound-agent-prompt",
        sessionId: String(SESSION_ID),
        inputSeq: 9n,
        expectedStatusEpoch: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        expectedOccupantId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        expectedRevision: 1n,
        text: "not written",
        budgetMs: 5_000,
      }),
    },
  }), false, socket);
  expect(sent).toEqual([{
    kind: "input-result",
    request_id: "unbound-agent-prompt",
    session_id: String(SESSION_ID),
    input_seq: 9n,
    status: TerminalInputStatus.REJECTED,
    written_bytes: 0,
    phase: TerminalWritePhase.PRE_WRITE,
    reason: "worker agent prompt handler is unavailable",
  }]);
});
