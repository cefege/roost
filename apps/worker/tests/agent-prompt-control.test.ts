// Focused agent-prompt tests cover shared text encoding, keeper outcome truth,
// downstream-hop validation, and the deliberate distinction from raw input.
// Fence races around the keeper admission queue live in the sibling test file.

import { create } from "@bufbuild/protobuf";
import { afterEach, describe, expect, test } from "bun:test";
import {
  DAgentPromptSchema,
  type DAgentPrompt,
} from "@roost/shared/proto/worker_transport_pb";
import { AGENT_PROMPT_MAX_TEXT_BYTES } from "@roost/shared/terminal-input";
import type { AgentProcessIdentity } from "../src/agent-status/process-scan.ts";
import {
  AgentStatusRegistry,
  type AgentStatusPrivateProof,
} from "../src/agent-status/registry.ts";
import {
  writeAgentPrompt,
  type AgentPromptControlDeps,
} from "../src/agent-prompt-control.ts";
import { acquireKeeperAdmission } from "../src/session-control-lanes.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import { MuxFrameType } from "../src/keeper/protocol.ts";
import { TERMINAL_REQUEST_BUDGET_CAP_MS } from "../src/transport/coord-link-constants.ts";
import type { TerminalRequestBudget } from "../src/transport/coord-link-types.ts";
import {
  installFakeKeeper,
  type FakeKeeper,
  type KeeperWrite,
} from "./keeper-fake-pool.ts";
import {
  CHANNEL_ID,
  cleanupStreamHarnesses,
  makeHarness,
  SESSION_ID,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";

const PROCESS_PROOF: AgentProcessIdentity = { agentId: "omp", pid: 4_242 };
const STATUS_MESSAGE_SECRET = "status-message-must-not-leave-worker";
const registries: AgentStatusRegistry[] = [];

interface PromptHarness {
  deps: AgentPromptControlDeps;
  proof: AgentStatusPrivateProof;
  refreshCalls: () => number;
  record: SessionShellRecord;
}

interface PromptPatch {
  requestId?: string;
  sessionId?: string;
  inputSeq?: bigint;
  expectedStatusEpoch?: string;
  expectedOccupantId?: string;
  expectedRevision?: bigint;
  text?: string;
  budgetMs?: number;
}


function requestFor(proof: AgentStatusPrivateProof, patch: PromptPatch = {}): DAgentPrompt {
  return create(DAgentPromptSchema, {
    requestId: patch.requestId ?? "agent-prompt-request",
    sessionId: patch.sessionId ?? String(SESSION_ID),
    inputSeq: patch.inputSeq ?? 1n,
    expectedStatusEpoch: patch.expectedStatusEpoch ?? proof.statusEpoch,
    expectedOccupantId: patch.expectedOccupantId ?? proof.occupantId,
    expectedRevision: patch.expectedRevision ?? BigInt(proof.revision),
    text: patch.text ?? "continue",
    budgetMs: patch.budgetMs ?? 5_000,
  });
}

function liveBudget(state = { current: true, remainingMs: 5_000 }): TerminalRequestBudget {
  return {
    isCurrentConnection: () => state.current,
    remainingMs: () => state.remainingMs,
  };
}

async function promptHarness(
  onRefresh?: (
    call: number,
    sessionId: string,
    processId: number,
    record: SessionShellRecord,
  ) => AgentProcessIdentity | null | Promise<AgentProcessIdentity | null>,
): Promise<PromptHarness> {
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
    agentId: PROCESS_PROOF.agentId,
    processId: PROCESS_PROOF.pid,
    state: "idle",
    message: STATUS_MESSAGE_SECRET,
    seq: 1,
    active: true,
  });
  const proof = registry.currentPrivateProof(String(SESSION_ID));
  if (!proof) throw new Error("prompt test status proof was not created");
  let refreshCalls = 0;
  const detector = {
    reportingAgentForSession: async (sessionId: string, processId: number) => {
      refreshCalls += 1;
      if (onRefresh) return onRefresh(refreshCalls, sessionId, processId, stream.record);
      return sessionId === String(SESSION_ID) && processId === PROCESS_PROOF.pid
        ? PROCESS_PROOF
        : null;
    },
  };
  return {
    deps: { sessions: stream.manager, registry, detector },
    proof,
    refreshCalls: () => refreshCalls,
    record: stream.record,
  };
}

function settlingKeeper(
  settle: (keeper: FakeKeeper, write: KeeperWrite) => void,
): FakeKeeper {
  let keeper!: FakeKeeper;
  keeper = trackKeeper(installFakeKeeper({
    onWrite: (write) => {
      if (write.type === MuxFrameType.PtyInRequest) settle(keeper, write);
    },
  }));
  return keeper;
}


afterEach(() => {
  for (const registry of registries.splice(0)) registry.dispose();
  cleanupStreamHarnesses();
});

describe("agent prompt terminal input ownership", () => {
  test("uses the final bracketed-paste mode while raw input remains byte-exact and logs omit secrets", async () => {
    const capturedLogs: string[] = [];
    const originalLog = console.log;
    const originalError = console.error;
    console.log = (...values: unknown[]) => { capturedLogs.push(values.map(String).join(" ")); };
    console.error = (...values: unknown[]) => { capturedLogs.push(values.map(String).join(" ")); };
    let promptResult;
    let rawResult;
    let keeper!: FakeKeeper;
    const promptText = "prompt-secret\r\nsecond\x1bZ";
    const rawBytes = new TextEncoder().encode("raw\n\x1b[31m");
    const expectedPromptBytes = new TextEncoder()
      .encode("\x1b[200~prompt-secret\rsecondZ\x1b[201~\r");
    try {
      const harness = await promptHarness((call, _sessionId, _processId, record) => {
        if (call === 2) record.wtermCore.writeString("\x1b[?2004h");
        return PROCESS_PROOF;
      });
      expect("message" in harness.proof).toBe(false);
      keeper = settlingKeeper((fake, write) => {
        fake.inputAck(write.channelId, write.seq!, write.bytes!.byteLength);
      });
      promptResult = await writeAgentPrompt(
        requestFor(harness.proof, { text: promptText }),
        liveBudget(),
        harness.deps,
      );
      rawResult = await harness.deps.sessions.writeTerminalInput(
        String(SESSION_ID),
        2n,
        rawBytes,
        liveBudget(),
      );
    } finally {
      console.log = originalLog;
      console.error = originalError;
    }

    expect(promptResult).toEqual({
      status: "accepted",
      writtenBytes: expectedPromptBytes.byteLength,
    });
    expect(rawResult).toEqual({ status: "accepted", writtenBytes: rawBytes.byteLength });
    const writes = keeper.writes.filter((write) => write.type === MuxFrameType.PtyInRequest);
    expect(new TextDecoder().decode(writes[0]!.bytes!))
      .toBe("\x1b[200~prompt-secret\rsecondZ\x1b[201~\r");
    expect(writes[1]!.bytes).toEqual(rawBytes);
    expect(capturedLogs.join("\n")).not.toContain(promptText);
    expect(capturedLogs.join("\n")).not.toContain(STATUS_MESSAGE_SECRET);
  });

  test("enforces UTF-8 bytes, safe revision, and the downstream-hop budget before scanning or writing", async () => {
    const harness = await promptHarness();
    const keeper = settlingKeeper((fake, write) => {
      fake.inputAck(write.channelId, write.seq!, write.bytes!.byteLength);
    });
    for (const patch of [
      { requestId: "" },
      { sessionId: "not-a-session" },
      { inputSeq: 0n },
      { expectedStatusEpoch: "not-an-epoch" },
      { expectedOccupantId: "not-an-occupant" },
    ] satisfies PromptPatch[]) {
      const result = await writeAgentPrompt(
        requestFor(harness.proof, patch),
        liveBudget(),
        harness.deps,
      );
      expect(result.status).toBe("rejected");
      expect(result.writtenBytes).toBe(0);
    }
    const overCap = "🐙".repeat(AGENT_PROMPT_MAX_TEXT_BYTES / 4 + 1);
    expect(await writeAgentPrompt(
      requestFor(harness.proof, { text: overCap }),
      liveBudget(),
      harness.deps,
    )).toEqual({ status: "rejected", writtenBytes: 0, reason: "prompt text is invalid" });
    expect(await writeAgentPrompt(
      requestFor(harness.proof, { expectedRevision: BigInt(Number.MAX_SAFE_INTEGER) + 1n }),
      liveBudget(),
      harness.deps,
    )).toEqual({ status: "rejected", writtenBytes: 0, reason: "expected_revision must be a safe uint64" });
    expect(await writeAgentPrompt(
      requestFor(harness.proof, { budgetMs: TERMINAL_REQUEST_BUDGET_CAP_MS + 1 }),
      liveBudget(),
      harness.deps,
    )).toEqual({ status: "rejected", writtenBytes: 0, reason: "budget_ms is invalid" });
    expect(harness.refreshCalls()).toBe(0);
    expect(keeper.writes).toHaveLength(0);

    const atCap = "🐙".repeat(AGENT_PROMPT_MAX_TEXT_BYTES / 4);
    expect(await writeAgentPrompt(
      requestFor(harness.proof, { text: atCap }),
      liveBudget(),
      harness.deps,
    )).toEqual({ status: "accepted", writtenBytes: AGENT_PROMPT_MAX_TEXT_BYTES + 1 });
    expect(harness.refreshCalls()).toBe(2);
    expect(keeper.writes).toHaveLength(1);
  });

  test("expires behind an unresolved predecessor without losing receive order", async () => {
    const harness = await promptHarness();
    const keeper = settlingKeeper((fake, write) => {
      fake.inputAck(write.channelId, write.seq!, write.bytes!.byteLength);
    });
    const predecessor = acquireKeeperAdmission(
      harness.deps.sessions,
      CHANNEL_ID,
      "terminal_resize",
    );
    await predecessor.granted;
    const deadlineAt = performance.now() + 50;
    const promptResult = writeAgentPrompt(
      requestFor(harness.proof),
      {
        isCurrentConnection: () => true,
        remainingMs: () => deadlineAt - performance.now(),
      },
      harness.deps,
    );
    const rawBytes = new TextEncoder().encode("later-raw-input");
    const rawResult = harness.deps.sessions.writeTerminalInput(
      String(SESSION_ID),
      2n,
      rawBytes,
      liveBudget(),
    );

    expect(await promptResult).toEqual({
      status: "rejected",
      writtenBytes: 0,
      reason: "prompt budget expired",
    });
    expect(harness.refreshCalls()).toBe(1);
    expect(keeper.writes).toHaveLength(0);
    predecessor.release();
    expect(await rawResult).toEqual({
      status: "accepted",
      writtenBytes: rawBytes.byteLength,
    });
    expect(keeper.writes.filter((write) => write.type === MuxFrameType.PtyInRequest))
      .toHaveLength(1);
    expect(keeper.writes[0]!.bytes).toEqual(rawBytes);
  });

  test("preserves keeper rejected, partial, and unknown truth without retrying or exposing reasons", async () => {
    const harness = await promptHarness();
    let outcome = 0;
    const keeper = settlingKeeper((fake, write) => {
      outcome += 1;
      if (outcome === 1) {
        fake.inputReject(write.channelId, write.seq!, "queue_full");
      } else if (outcome === 2) {
        fake.inputAmbiguous(write.channelId, write.seq!, {
          writtenBytes: 2,
          reason: "invalid_write_count",
        });
      } else {
        fake.inputAmbiguous(write.channelId, write.seq!, {
          writtenBytes: null,
          reason: "disconnected",
        });
      }
    });

    expect(await writeAgentPrompt(
      requestFor(harness.proof, { inputSeq: 1n, text: "reject" }),
      liveBudget(),
      harness.deps,
    )).toEqual({ status: "rejected", writtenBytes: 0, reason: "keeper rejected the agent prompt" });
    expect(await writeAgentPrompt(
      requestFor(harness.proof, { inputSeq: 2n, text: "partial" }),
      liveBudget(),
      harness.deps,
    )).toEqual({
      status: "ambiguous",
      writtenBytes: 2,
      reason: "keeper agent prompt outcome is ambiguous",
    });
    expect(await writeAgentPrompt(
      requestFor(harness.proof, { inputSeq: 3n, text: "unknown" }),
      liveBudget(),
      harness.deps,
    )).toEqual({
      status: "ambiguous",
      writtenBytes: 0,
      reason: "keeper agent prompt outcome is ambiguous",
    });
    await Promise.resolve();
    expect(keeper.writes.filter((write) => write.type === MuxFrameType.PtyInRequest)).toHaveLength(3);
  });

});
