// The foreground-job fence: a prompt is admitted only while the pane's tty
// foreground job belongs to the agent's own process subtree. The proof comes
// from the real process scanner reading a synthetic `ps` snapshot, so the
// fence runs for real; other prompt fences live in agent-prompt-fences.test.ts.

import { create } from "@bufbuild/protobuf";
import { afterEach, describe, expect, test } from "bun:test";
import {
  DAgentPromptSchema,
  type DAgentPrompt,
} from "@roost/shared/proto/worker_transport_pb";
import {
  AgentProcessScanner,
  parsePsSnapshot,
} from "../src/agent-status/process-scan.ts";
import {
  AgentStatusRegistry,
  type AgentStatusPrivateProof,
} from "../src/agent-status/registry.ts";
import {
  writeAgentPrompt,
  type AgentPromptControlDeps,
} from "../src/agent-prompt-control.ts";
import { MuxFrameType } from "../src/keeper/protocol.ts";
import type { TerminalRequestBudget } from "../src/transport/coord-link-types.ts";
import { installFakeKeeper, type FakeKeeper } from "./keeper-fake-pool.ts";
import {
  cleanupStreamHarnesses,
  makeHarness,
  SESSION_ID,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";

const PANE_CHILD_PID = 100;
const AGENT_PID = 200;
const registries: AgentStatusRegistry[] = [];

interface PaneProcess {
  pid: number;
  ppid: number;
  pgid: number;
  tpgid: number;
  comm: string;
}

interface ForegroundHarness {
  deps: AgentPromptControlDeps;
  proof: AgentStatusPrivateProof;
}

function requestFor(proof: AgentStatusPrivateProof): DAgentPrompt {
  return create(DAgentPromptSchema, {
    requestId: "agent-prompt-foreground",
    sessionId: String(SESSION_ID),
    inputSeq: 1n,
    expectedStatusEpoch: proof.statusEpoch,
    expectedOccupantId: proof.occupantId,
    expectedRevision: BigInt(proof.revision),
    text: "continue",
    budgetMs: 5_000,
  });
}

function liveBudget(): TerminalRequestBudget {
  return { isCurrentConnection: () => true, remainingMs: () => 5_000 };
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

/** A live session whose process proof is scanned out of `pane`, formatted the
 *  way `ps -o pid=,ppid=,pgid=,tpgid=,comm=,args=` reports it. */
async function foregroundHarness(pane: readonly PaneProcess[]): Promise<ForegroundHarness> {
  const stream = await makeHarness();
  stream.record.childPid = PANE_CHILD_PID;
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
    processId: AGENT_PID,
    state: "idle",
    seq: 1,
    active: true,
  });
  const proof = registry.currentPrivateProof(String(SESSION_ID));
  if (!proof) throw new Error("foreground fence status proof was not created");
  const snapshot = pane
    .map((row) => `${row.pid} ${row.ppid} ${row.pgid} ${row.tpgid} ${row.comm} ${row.comm}`)
    .join("\n");
  const scanner = new AgentProcessScanner(async () => parsePsSnapshot(snapshot), 0);
  const root = { sessionId: String(SESSION_ID), childPid: PANE_CHILD_PID };
  const discovered = await scanner.scanAgents([root]);
  if (discovered.get(root.sessionId)?.pid !== AGENT_PID) {
    throw new Error("foreground fence harness did not discover the agent process");
  }
  const detector = {
    reportingAgentForSession: async (
      _sessionId: string,
      processId: number,
      signal?: AbortSignal,
    ) => scanner.scanReportingAgent(root, processId, signal),
  };
  return { deps: { sessions: stream.manager, registry, detector }, proof };
}

afterEach(() => {
  for (const registry of registries.splice(0)) registry.dispose();
  cleanupStreamHarnesses();
});

describe("agent prompt foreground ownership", () => {
  test("admits the agent holding the pane foreground job, tool subprocess included", async () => {
    if (process.platform === "win32") return;
    const keeper = inputAckKeeper();
    for (const pane of [
      [
        { pid: PANE_CHILD_PID, ppid: 1, pgid: PANE_CHILD_PID, tpgid: AGENT_PID, comm: "bash" },
        { pid: AGENT_PID, ppid: PANE_CHILD_PID, pgid: AGENT_PID, tpgid: AGENT_PID, comm: "omp" },
      ],
      [
        { pid: PANE_CHILD_PID, ppid: 1, pgid: PANE_CHILD_PID, tpgid: 300, comm: "bash" },
        { pid: AGENT_PID, ppid: PANE_CHILD_PID, pgid: AGENT_PID, tpgid: 300, comm: "omp" },
        { pid: 300, ppid: AGENT_PID, pgid: 300, tpgid: 300, comm: "rg" },
      ],
    ]) {
      const harness = await foregroundHarness(pane);
      expect(await writeAgentPrompt(requestFor(harness.proof), liveBudget(), harness.deps))
        .toEqual({ status: "accepted", writtenBytes: 9 });
    }
    const writes = keeper.writes.filter((write) => write.type === MuxFrameType.PtyInRequest);
    expect(writes.map((write) => new TextDecoder().decode(write.bytes!)))
      .toEqual(["continue", "\r", "continue", "\r"]);
  });

  test("rejects when the pane foreground job is outside the agent subtree", async () => {
    if (process.platform === "win32") return;
    const keeper = inputAckKeeper();
    for (const pane of [
      [
        { pid: PANE_CHILD_PID, ppid: 1, pgid: PANE_CHILD_PID, tpgid: 400, comm: "bash" },
        { pid: AGENT_PID, ppid: PANE_CHILD_PID, pgid: AGENT_PID, tpgid: 400, comm: "omp" },
        { pid: 400, ppid: PANE_CHILD_PID, pgid: 400, tpgid: 400, comm: "vi" },
      ],
      [
        { pid: PANE_CHILD_PID, ppid: 1, pgid: PANE_CHILD_PID, tpgid: -1, comm: "bash" },
        { pid: AGENT_PID, ppid: PANE_CHILD_PID, pgid: AGENT_PID, tpgid: -1, comm: "omp" },
      ],
    ]) {
      const harness = await foregroundHarness(pane);
      expect(await writeAgentPrompt(requestFor(harness.proof), liveBudget(), harness.deps))
        .toEqual({
          status: "rejected",
          writtenBytes: 0,
          reason: "agent is not the terminal foreground process",
        });
    }
    expect(keeper.writes).toHaveLength(0);
  });
});
