// Shared fixtures for worker boot-reconciliation admission tests.
// They construct typed coordinator responses and controlled SessionManager operations.
// Test files retain scenario assertions while this module keeps their common setup bounded.
// Depends on the worker reconciliation and keeper test seams.

import { vi } from "bun:test";
import { tmpdir } from "node:os";
import { asSessionId, asWorkerFp } from "@roost/shared/wire";
import type { CoordClient } from "../src/coord-client.ts";
import {
  completeWorkerBootAdmission,
} from "../src/main.ts";
import type { ReconcileAdmissionOutcome } from "../src/boot-reconcile.ts";
import { SessionManager } from "../src/session-manager.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import { AgentReferenceAdmissionGate } from "../src/agent-status/reference-admission.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";

export const WORKER_FP = asWorkerFp("42".repeat(32));
export const OPEN_SESSIONS = [
  {
    id: asSessionId("00000000-0000-4000-8000-000000000001"),
    channel: 11,
    kind: "shell",
    cwd: tmpdir(),
  },
  {
    id: asSessionId("00000000-0000-4000-8000-000000000002"),
    channel: 12,
    kind: "shell",
    cwd: tmpdir(),
  },
] as const;

export function clientWithSessionsList(
  sessionsList: (
    ...args: Parameters<CoordClient["sessionsList"]>
  ) => Promise<unknown>,
): CoordClient {
  return { sessionsList } as unknown as CoordClient;
}

export function referenceReconcileDependencies() {
  return {
    referenceAdmission: new AgentReferenceAdmissionGate(),
    restoreAgentConversation: async () => ({
      status: "skipped" as const,
      reason: "disabled" as const,
    }),
    beforeRecoveryRead: async () => {},
  };
}

export function sessionsResponse<
  const Sessions extends readonly { id: string }[],
>(sessions: Sessions) {
  return {
    sessions,
    recoveryMetadata: sessions.map((session) => ({
      sessionId: session.id,
      agentReference: undefined,
      agentReferenceClientSeq: 0n,
    })),
  };
}

export function stubSessionAdmission(manager: SessionManager) {
  const advance = vi.fn(async () => {});
  const resume = vi.fn(async (
    _options: Parameters<SessionManager["resume"]>[0],
    reservation: Parameters<SessionManager["resume"]>[1],
  ) => {
    if (!reservation) throw new Error("test reconcile omitted close reservation");
    manager.releaseSessionEvent(reservation);
    return true;
  });
  const respawn = vi.fn(async () => {});
  const reap = vi.fn(async () => 0);
  manager.advanceChannelCounterPastKeeper = advance;
  manager.resume = resume;
  manager.respawn = respawn;
  manager.reapStrayKeeperChannels = reap;
  return { advance, resume, respawn, reap };
}

export function spyOnKeeperMutation() {
  const pool = getMultiplexedPool();
  const ensure = vi.spyOn(pool, "ensure").mockResolvedValue();
  const list = vi.spyOn(pool, "listChannels").mockResolvedValue([]);
  const listFresh = vi.spyOn(pool, "listChannelsFresh").mockResolvedValue([]);
  const kill = vi.spyOn(pool, "kill").mockImplementation(() => {});
  return { ensure, list, listFresh, kill };
}

export function bootActivation(reconcile: () => Promise<ReconcileAdmissionOutcome>) {
  const activateSnapshotProvider = vi.fn();
  const markReady = vi.fn();
  return {
    activateSnapshotProvider,
    markReady,
    complete: () => completeWorkerBootAdmission({
      reconcile,
      activateSnapshotProvider,
      markReady,
    }),
  };
}
