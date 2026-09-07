// Pins the worker-authored conversation-reference clear. When the agent process
// leaves a still-live session, the detector appends exactly one durable
// `reference: null`, so a later restore cannot type a resume for a conversation
// the user already ended. Exercises AgentScreenDetector against a scripted
// process scanner and a recording event sink.
import { describe, expect, test } from "bun:test";
import { DEFAULT_COLOR } from "@roost/shared/cell";
import { asSessionId, type SessionEvent } from "@roost/shared/wire";
import type { CellData, TerminalCore } from "@wterm/core";
import type { SessionManager } from "../src/session-manager.ts";
import {
  AgentScreenDetector,
  type AgentReferenceClearDeps,
} from "../src/agent-status/detector.ts";
import { AgentReferenceAdmissionGate } from "../src/agent-status/reference-admission.ts";
import {
  AgentProcessScanner,
  type BuiltinAgentId,
} from "../src/agent-status/process-scan.ts";
import { AgentStatusRegistry } from "../src/agent-status/registry.ts";
import type { SessionEventReservation } from "../src/event-sink.ts";

const SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");

function makeGridCore(): TerminalCore {
  const blank: CellData = {
    char: " ".codePointAt(0)!,
    fg: DEFAULT_COLOR,
    bg: DEFAULT_COLOR,
    flags: 0,
    fgRgb: undefined,
    bgRgb: undefined,
  };
  return {
    getCols: () => 8,
    getRows: () => 2,
    getCell: () => blank,
  } as unknown as TerminalCore;
}

function makeHarness() {
  const records: Array<Record<string, unknown>> = [{
    sessionId: SESSION_ID,
    channelId: 7,
    childPid: 4_321,
    wtermCore: makeGridCore(),
    rawOscTitle: null,
    rawOscProgress: null,
  }];
  const sessions = {
    allSessions: () => records,
    getBySessionId: (id: string) => records.find((record) => record.sessionId === id),
  } as unknown as SessionManager;
  const registry = new AgentStatusRegistry({
    publish: () => {},
    startLeaseTimer: false,
  });
  const identities = new Map<string, { agentId: BuiltinAgentId; pid: number }>([
    [SESSION_ID, { agentId: "omp", pid: 4_321 }],
  ]);
  const scanner = {
    scanAgents: async () => identities,
    scanReportingAgent: async () => null,
  } as unknown as AgentProcessScanner;
  const emitted: SessionEvent[] = [];
  const releases = { count: 0 };
  const referenceClear: AgentReferenceClearDeps = {
    eventSink: {
      reserveSessionEvent: (kind) => (
        { kind, payloadBytes: 0 } as unknown as SessionEventReservation
      ),
      releaseSessionEvent: () => { releases.count++; },
      emit: (event) => { emitted.push(event); },
    },
    referenceAdmission: new AgentReferenceAdmissionGate(),
  };
  const detector = new AgentScreenDetector(sessions, registry, scanner, {
    referenceClear,
  });
  return { detector, identities, emitted, records, releases };
}

/** Two passes: the first may join a scan already in flight, so only the second
 *  is guaranteed to have observed the latest scanner state. The microtask drain
 *  covers the admission gate's turn hop. */
async function scanTwice(detector: AgentScreenDetector): Promise<void> {
  await detector.scanNow();
  await detector.scanNow();
  for (let tick = 0; tick < 8; tick++) await Promise.resolve();
}

describe("agent-status conversation-reference clear", () => {
  test("an omp process leaving a live session clears the reference exactly once", async () => {
    const { detector, identities, emitted, releases } = makeHarness();
    await scanTwice(detector);
    expect(emitted).toEqual([]);

    identities.delete(SESSION_ID);
    await scanTwice(detector);
    expect(emitted).toEqual([{
      kind: "agent_reference",
      session_id: SESSION_ID,
      reference: null,
      ts: expect.any(Number),
    }]);

    await scanTwice(detector);
    await scanTwice(detector);
    expect(emitted).toHaveLength(1);
    expect(releases.count).toBe(0);
    detector.dispose();
  });

  test("a fresh agent on the same session id clears again when it leaves", async () => {
    const { detector, identities, emitted } = makeHarness();
    await scanTwice(detector);
    identities.delete(SESSION_ID);
    await scanTwice(detector);
    identities.set(SESSION_ID, { agentId: "omp", pid: 5_555 });
    await scanTwice(detector);
    identities.delete(SESSION_ID);
    await scanTwice(detector);
    expect(emitted).toHaveLength(2);
    expect(emitted.every((event) => event.kind === "agent_reference"
      && event.reference === null)).toBe(true);
    detector.dispose();
  });

  test("a session that never ran omp is never cleared", async () => {
    const { detector, identities, emitted } = makeHarness();
    identities.set(SESSION_ID, { agentId: "pi", pid: 4_321 });
    await scanTwice(detector);
    identities.delete(SESSION_ID);
    await scanTwice(detector);
    await scanTwice(detector);
    expect(emitted).toEqual([]);
    detector.dispose();
  });

  test("a session that left the manager is not cleared", async () => {
    const { detector, identities, emitted, records } = makeHarness();
    await scanTwice(detector);
    records.splice(0, records.length);
    identities.delete(SESSION_ID);
    await scanTwice(detector);
    expect(emitted).toEqual([]);
    detector.dispose();
  });
});
