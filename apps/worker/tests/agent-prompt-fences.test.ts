// Agent-prompt fence tests hold the real terminal-input admission lane while
// session, status, process, connection, and deadline proofs change. Every race
// must settle as a definite zero-write rejection before keeper admission.

import { create } from "@bufbuild/protobuf";
import { afterEach, describe, expect, test } from "bun:test";
import {
  DAgentPromptSchema,
  type DAgentPrompt,
} from "@roost/shared/proto/worker_transport_pb";
import type { AgentProcessIdentity } from "../src/agent-status/process-scan.ts";
import {
  AgentStatusRegistry,
  type AgentStatusPrivateProof,
} from "../src/agent-status/registry.ts";
import {
  writeAgentPrompt,
  type AgentPromptControlDeps,
} from "../src/agent-prompt-control.ts";
import { MuxFrameType } from "../src/keeper/protocol.ts";
import { acquireKeeperAdmission } from "../src/session-control-lanes.ts";
import type { SessionManager } from "../src/session-manager.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import type { TerminalRequestBudget } from "../src/transport/coord-link-types.ts";
import { installFakeKeeper, type FakeKeeper } from "./keeper-fake-pool.ts";
import {
  CHANNEL_ID,
  cleanupStreamHarnesses,
  makeHarness,
  SESSION_ID,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";

const PROCESS_PROOF: AgentProcessIdentity = {
  agentId: "omp",
  pid: 7_321,
  foreground: { groupId: 7_321, agentMemberPid: 7_321 },
};
const registries: AgentStatusRegistry[] = [];

interface FenceHarness {
  deps: AgentPromptControlDeps;
  registry: AgentStatusRegistry;
  proof: AgentStatusPrivateProof;
  refreshCalls: () => number;
}

interface StatusSetup {
  source?: "integration" | "screen";
  state?: "idle" | "working" | "blocked";
}

type RefreshHook = (
  call: number,
  record: SessionShellRecord,
  signal?: AbortSignal,
) => AgentProcessIdentity | null | Promise<AgentProcessIdentity | null>;

function requestFor(proof: AgentStatusPrivateProof): DAgentPrompt {
  return create(DAgentPromptSchema, {
    requestId: "agent-prompt-fence",
    sessionId: String(SESSION_ID),
    inputSeq: 1n,
    expectedStatusEpoch: proof.statusEpoch,
    expectedOccupantId: proof.occupantId,
    expectedRevision: BigInt(proof.revision),
    text: "continue",
    budgetMs: 5_000,
  });
}

function liveBudget(state: { current: boolean; remainingMs: number }): TerminalRequestBudget {
  return {
    isCurrentConnection: () => state.current,
    remainingMs: () => state.remainingMs,
  };
}

async function fenceHarness(
  status: StatusSetup = {},
  onRefresh?: RefreshHook,
): Promise<FenceHarness> {
  const stream = await makeHarness();
  stream.record.childPid = 100;
  const registry = new AgentStatusRegistry({
    publish: () => {},
    now: () => 1_000,
    leaseMs: 60_000,
    startLeaseTimer: false,
  });
  registries.push(registry);
  if (status.source === "screen") {
    registry.reportScreen(String(SESSION_ID), {
      agentId: "omp",
      processId: PROCESS_PROOF.pid,
      state: status.state ?? "idle",
      visibleBlocker: false,
    });
  } else {
    registry.reportIntegration({
      sessionId: String(SESSION_ID),
      agentId: "omp",
      processId: PROCESS_PROOF.pid,
      state: status.state ?? "idle",
      seq: 1,
      active: true,
    });
  }
  const proof = registry.currentPrivateProof(String(SESSION_ID));
  if (!proof) throw new Error("fence test status proof was not created");
  let refreshCalls = 0;
  const detector = {
    reportingAgentForSession: async (
      _sessionId: string,
      _processId: number,
      signal?: AbortSignal,
    ) => {
      refreshCalls += 1;
      return onRefresh
        ? onRefresh(refreshCalls, stream.record, signal)
        : PROCESS_PROOF;
    },
  };
  return {
    deps: { sessions: stream.manager, registry, detector },
    registry,
    proof,
    refreshCalls: () => refreshCalls,
  };
}

async function waitForQueuedPrompt(sessions: SessionManager): Promise<void> {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const lane = sessions.keeperAdmissionLane.get(CHANNEL_ID);
    if (lane?.depth === 1 && lane.holder === "terminal_resize") return;
    await Promise.resolve();
  }
  throw new Error("agent prompt did not enter the keeper admission queue");
}

function inputAckKeeper(): FakeKeeper {
  let keeper!: FakeKeeper;
  keeper = trackKeeper(installFakeKeeper({
    onWrite: (write) => {
      if (write.type === MuxFrameType.PtyInRequest) {
        keeper.inputAck(write.channelId, write.seq!, write.bytes!.byteLength);
      }
    },
  }));
  return keeper;
}

afterEach(() => {
  for (const registry of registries.splice(0)) registry.dispose();
  cleanupStreamHarnesses();
});

describe("agent prompt pre-write fences", () => {
  test("blocked and screen-only status reject without a process refresh", async () => {
    for (const status of [
      { source: "integration", state: "blocked" },
      { source: "screen", state: "idle" },
    ] as const) {
      const harness = await fenceHarness(status);
      const result = await writeAgentPrompt(
        requestFor(harness.proof),
        liveBudget({ current: true, remainingMs: 5_000 }),
        harness.deps,
      );
      expect(result).toEqual({
        status: "rejected",
        writtenBytes: 0,
        reason: status.source === "screen"
          ? "agent status source is not integration"
          : "agent is blocked",
      });
      expect(harness.refreshCalls()).toBe(0);
    }
  });

  test("reserves prompt receive order before the initial process scan settles", async () => {
    const initialScan = Promise.withResolvers<AgentProcessIdentity | null>();
    const harness = await fenceHarness({}, (call) => (
      call === 1 ? initialScan.promise : PROCESS_PROOF
    ));
    const keeper = inputAckKeeper();
    const promptResult = writeAgentPrompt(
      requestFor(harness.proof),
      liveBudget({ current: true, remainingMs: 5_000 }),
      harness.deps,
    );
    expect(harness.refreshCalls()).toBe(1);
    expect(harness.deps.sessions.keeperAdmissionLane.get(CHANNEL_ID))
      .toMatchObject({ depth: 1, holder: null });

    const rawBytes = new TextEncoder().encode("later-raw-input");
    const rawResult = harness.deps.sessions.writeTerminalInput(
      String(SESSION_ID),
      2n,
      rawBytes,
      liveBudget({ current: true, remainingMs: 5_000 }),
    );
    expect(harness.deps.sessions.keeperAdmissionLane.get(CHANNEL_ID)?.depth).toBe(2);
    initialScan.resolve(PROCESS_PROOF);

    expect(await Promise.all([promptResult, rawResult])).toEqual([
      { status: "accepted", writtenBytes: 9 },
      { status: "accepted", writtenBytes: rawBytes.byteLength },
    ]);
    const writes = keeper.writes.filter((write) => write.type === MuxFrameType.PtyInRequest);
    expect(writes.map((write) => new TextDecoder().decode(write.bytes!)))
      .toEqual(["continue", "\r", "later-raw-input"]);
  });

  test("drains a queued ticket when the initial process proof is rejected", async () => {
    const harness = await fenceHarness({}, () => null);
    const keeper = inputAckKeeper();
    const blocker = acquireKeeperAdmission(
      harness.deps.sessions,
      CHANNEL_ID,
      "terminal_resize",
    );
    await blocker.granted;
    const promptResult = writeAgentPrompt(
      requestFor(harness.proof),
      liveBudget({ current: true, remainingMs: 5_000 }),
      harness.deps,
    );
    const rawBytes = new TextEncoder().encode("raw-after-rejection");
    const rawResult = harness.deps.sessions.writeTerminalInput(
      String(SESSION_ID),
      2n,
      rawBytes,
      liveBudget({ current: true, remainingMs: 5_000 }),
    );
    expect(harness.deps.sessions.keeperAdmissionLane.get(CHANNEL_ID))
      .toMatchObject({ depth: 2, holder: "terminal_resize" });
    blocker.release();

    expect(await promptResult).toEqual({
      status: "rejected",
      writtenBytes: 0,
      reason: "agent process proof could not be refreshed",
    });
    expect(await rawResult).toEqual({
      status: "accepted",
      writtenBytes: rawBytes.byteLength,
    });
    expect(keeper.writes.filter((write) => write.type === MuxFrameType.PtyInRequest))
      .toHaveLength(1);
    const lane = harness.deps.sessions.keeperAdmissionLane.get(CHANNEL_ID);
    expect(lane?.depth ?? 0).toBe(0);
    expect(lane?.holder ?? null).toBeNull();
  });

  test("aborts a stalled final process scan and releases input at budget expiry", async () => {
    const finalScanStarted = Promise.withResolvers<void>();
    let finalScanAborted = false;
    const harness = await fenceHarness({}, (call, _record, signal) => {
      if (call === 1) return PROCESS_PROOF;
      finalScanStarted.resolve();
      return new Promise<AgentProcessIdentity | null>((_resolve, reject) => {
        const rejectAborted = () => {
          finalScanAborted = true;
          reject(new Error("forced process scan aborted"));
        };
        if (signal?.aborted) rejectAborted();
        else signal?.addEventListener("abort", rejectAborted, { once: true });
      });
    });
    const keeper = inputAckKeeper();
    const deadlineAt = performance.now() + 100;
    const budget: TerminalRequestBudget = {
      isCurrentConnection: () => true,
      remainingMs: () => deadlineAt - performance.now(),
    };
    const startedAt = performance.now();
    const promptResult = writeAgentPrompt(
      requestFor(harness.proof),
      budget,
      harness.deps,
    );
    await finalScanStarted.promise;
    const rawBytes = new TextEncoder().encode("raw-after-timeout");
    const rawResult = harness.deps.sessions.writeTerminalInput(
      String(SESSION_ID),
      2n,
      rawBytes,
      liveBudget({ current: true, remainingMs: 5_000 }),
    );

    expect(await promptResult).toEqual({
      status: "rejected",
      writtenBytes: 0,
      reason: "prompt budget expired",
    });
    expect(await rawResult).toEqual({
      status: "accepted",
      writtenBytes: rawBytes.byteLength,
    });
    expect(performance.now() - startedAt).toBeLessThan(1_000);
    expect(finalScanAborted).toBe(true);
    const writes = keeper.writes.filter((write) => write.type === MuxFrameType.PtyInRequest);
    expect(writes).toHaveLength(1);
    expect(writes[0]!.bytes).toEqual(rawBytes);
    const lane = harness.deps.sessions.keeperAdmissionLane.get(CHANNEL_ID);
    expect(lane?.depth ?? 0).toBe(0);
    expect(lane?.holder ?? null).toBeNull();
  });

  test("safe stale revision, occupant replacement, and closure reject after queueing", async () => {
    const keeper = trackKeeper(installFakeKeeper());
    for (const mutation of ["revision", "replacement", "closed"] as const) {
      const harness = await fenceHarness();
      const blocker = acquireKeeperAdmission(
        harness.deps.sessions,
        CHANNEL_ID,
        "terminal_resize",
      );
      await blocker.granted;
      const resultPromise = writeAgentPrompt(
        requestFor(harness.proof),
        liveBudget({ current: true, remainingMs: 5_000 }),
        harness.deps,
      );
      await waitForQueuedPrompt(harness.deps.sessions);
      if (mutation === "revision") {
        harness.registry.reportIntegration({
          sessionId: String(SESSION_ID),
          agentId: "omp",
          processId: PROCESS_PROOF.pid,
          state: "working",
          seq: 2,
          active: true,
        });
        const changed = harness.registry.currentPrivateProof(String(SESSION_ID));
        expect(changed?.occupantId).toBe(harness.proof.occupantId);
        expect(changed?.revision).not.toBe(harness.proof.revision);
      } else if (mutation === "replacement") {
        harness.registry.reportIntegration({
          sessionId: String(SESSION_ID),
          agentId: "omp",
          processId: PROCESS_PROOF.pid + 1,
          state: "idle",
          seq: 1,
          active: true,
        });
      } else {
        harness.registry.closeSession(String(SESSION_ID));
        harness.deps.sessions.sessions.delete(CHANNEL_ID);
      }
      blocker.release();
      const result = await resultPromise;
      expect(result).toEqual({
        status: "rejected",
        writtenBytes: 0,
        reason: mutation === "closed"
          ? "session changed before prompt admission"
          : "agent status fence changed",
      });
      expect(harness.refreshCalls()).toBe(1);
    }
    expect(keeper.writes).toHaveLength(0);
  });

  test("final deadline, current connection, and refreshed process proof all gate the write", async () => {
    const keeper = trackKeeper(installFakeKeeper());
    const reasons = {
      deadline: "prompt budget expired",
      connection: "worker connection was superseded",
      process: "agent process proof changed before the keeper write",
    } as const;
    for (const failure of ["deadline", "connection", "process"] as const) {
      const budgetState = { current: true, remainingMs: 5_000 };
      const harness = await fenceHarness({}, (call) => {
        if (call === 2 && failure === "deadline") budgetState.remainingMs = 0;
        if (call === 2 && failure === "connection") budgetState.current = false;
        if (call === 2 && failure === "process") {
          return { agentId: "omp", pid: PROCESS_PROOF.pid + 1 };
        }
        return PROCESS_PROOF;
      });
      const result = await writeAgentPrompt(
        requestFor(harness.proof),
        liveBudget(budgetState),
        harness.deps,
      );
      expect(result).toEqual({
        status: "rejected",
        writtenBytes: 0,
        reason: reasons[failure],
      });
      expect(harness.refreshCalls()).toBe(2);
    }
    expect(keeper.writes).toHaveLength(0);
  });
});
