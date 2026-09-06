// Boot reconciliation may resume an OMP conversation only after keeper adoption
// failed and the ordinary replacement shell's `respawned` event is durable.
// One reference is claimed once per pass, and outcomes never re-enter respawn.

import { afterEach, expect, test, vi } from "bun:test";
import { tmpdir } from "node:os";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import type { AgentConversationReferenceV1 } from "@roost/shared/agent-conversation-reference";
import type { CoordClient } from "../src/coord-client.ts";
import { reconcileCoordinatorSessions } from "../src/boot-session-reconcile.ts";
import {
  conversationRestoreDedupeKey,
  type AgentConversationRestoreOutcome,
} from "../src/agent-conversation-restore.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import { SessionManager } from "../src/session-manager.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";

const WORKER_FP = asWorkerFp("42".repeat(32));
const SESSION_ID = asSessionId("00000000-0000-4000-8000-000000000051");
const SECOND_SESSION_ID = asSessionId("00000000-0000-4000-8000-000000000052");
const RESPAWN_CHANNEL_ID = asChannelId(5_100);
const AGENT_REFERENCE: AgentConversationReferenceV1 = {
  schema_version: 1,
  agent_id: "omp",
  kind: "path",
  value: "/private/opaque-secret.jsonl",
};

const pool = getMultiplexedPool();
const managers: SessionManager[] = [];

afterEach(() => {
  for (const manager of managers) manager.dispose();
  managers.length = 0;
  vi.restoreAllMocks();
});

function openSessionRows(count: number) {
  const ids = [SESSION_ID, SECOND_SESSION_ID].slice(0, count);
  return ids.map((id, index) => ({
    id,
    channel: 11 + index,
    kind: "shell",
    cwd: tmpdir(),
  }));
}

function clientFor(sessionCount: number): () => CoordClient {
  const sessions = openSessionRows(sessionCount);
  return () => ({
    sessionsList: async () => ({
      sessions,
      recoveryMetadata: sessions.map((session) => ({
        sessionId: session.id,
        agentReference: {
          schemaVersion: AGENT_REFERENCE.schema_version,
          agentId: AGENT_REFERENCE.agent_id,
          kind: AGENT_REFERENCE.kind,
          value: AGENT_REFERENCE.value,
        },
        agentReferenceClientSeq: 1n,
      })),
    }),
  }) as unknown as CoordClient;
}

/** Adoption result is the only branch under test; keeper IO stays stubbed.
 * `adopts` may vary per session id to mix adoption and respawn in one pass. */
function managerWithReconcileStubs(
  adopts: boolean | ((sessionId: string) => boolean),
  order: string[] = [],
) {
  const sink = new SessionEventTestSink();
  const manager = new SessionManager({ workerFp: WORKER_FP, sink });
  managers.push(manager);
  vi.spyOn(pool, "ensure").mockResolvedValue();
  vi.spyOn(pool, "listChannels").mockResolvedValue([]);
  vi.spyOn(pool, "listChannelsFresh").mockResolvedValue([]);
  vi.spyOn(pool, "kill").mockImplementation(() => {});
  manager.advanceChannelCounterPastKeeper = vi.fn(async () => {});
  manager.startPostAdmissionMaintenance = vi.fn(async () => {});
  manager.reapStrayKeeperChannels = vi.fn(async () => 0);
  const resume = vi.fn(async (
    options: Parameters<SessionManager["resume"]>[0],
    reservation: Parameters<SessionManager["resume"]>[1],
  ) => {
    if (!reservation) throw new Error("test reconcile omitted close reservation");
    manager.releaseSessionEvent(reservation);
    return typeof adopts === "function"
      ? adopts(String(options.sessionId))
      : adopts;
  });
  const respawn = vi.fn(async (
    options: Parameters<SessionManager["respawn"]>[0],
    reservations: Parameters<SessionManager["respawn"]>[1],
  ) => {
    if (!reservations) throw new Error("test respawn omitted reservations");
    order.push("respawn_event_durable");
    manager.emitEvent({
      kind: "respawned",
      session_id: options.oldSessionId,
      new_channel: RESPAWN_CHANNEL_ID,
      ts: Date.now(),
    }, reservations.event);
    manager.releaseSessionEvent(reservations.close);
    order.push("respawn_returned");
  });
  const tombstone = vi.fn();
  manager.resume = resume;
  manager.respawn = respawn;
  manager.emitClosedTombstone = tombstone;
  return { manager, sink, resume, respawn, tombstone };
}

function reconcile(
  manager: SessionManager,
  restoreAgentConversation: Parameters<
    typeof reconcileCoordinatorSessions
  >[0]["restoreAgentConversation"],
  sessionCount = 1,
) {
  return reconcileCoordinatorSessions({
    client: clientFor(sessionCount),
    workerFp: WORKER_FP,
    sessionMgr: manager,
    referenceRecoveryAdmission: async <Result>(read: () => Promise<Result>) =>
      read(),
    prepareKeeper: async () => {},
    restoreAgentConversation,
  }, "boot");
}

test("successful keeper adoption performs zero restore input", async () => {
  const harness = managerWithReconcileStubs(true);
  const restore = vi.fn(async (): Promise<AgentConversationRestoreOutcome> => ({
    status: "accepted",
    writtenBytes: 1,
  }));

  await expect(reconcile(harness.manager, restore)).resolves.toMatchObject({
    admitted: true,
    resumed: 1,
    respawned: 0,
  });
  expect(harness.respawn).not.toHaveBeenCalled();
  expect(restore).not.toHaveBeenCalled();
});

test("restore waits for the durable respawn operation to return", async () => {
  const order: string[] = [];
  const harness = managerWithReconcileStubs(false, order);
  const allowRespawnReturn = Promise.withResolvers<void>();
  const respawnStarted = Promise.withResolvers<void>();
  const originalRespawn = harness.manager.respawn.bind(harness.manager);
  harness.manager.respawn = vi.fn(async (
    options: Parameters<SessionManager["respawn"]>[0],
    reservations: Parameters<SessionManager["respawn"]>[1],
  ) => {
    order.push("respawn_started");
    respawnStarted.resolve();
    await allowRespawnReturn.promise;
    await originalRespawn(options, reservations);
  });
  const restore = vi.fn(async (): Promise<AgentConversationRestoreOutcome> => {
    order.push("restore_started");
    expect(harness.sink.events.at(-1)?.kind).toBe("respawned");
    return { status: "accepted", writtenBytes: 1 };
  });

  const admission = reconcile(harness.manager, restore);
  await respawnStarted.promise;
  expect(restore).not.toHaveBeenCalled();
  allowRespawnReturn.resolve();
  await expect(admission).resolves.toMatchObject({
    admitted: true,
    respawned: 1,
  });
  expect(order).toEqual([
    "respawn_started",
    "respawn_event_durable",
    "respawn_returned",
    "restore_started",
  ]);
});

for (const outcome of [
  { status: "accepted", writtenBytes: 42 },
  { status: "rejected", writtenBytes: 0, reason: "keeper rejected" },
  { status: "ambiguous", writtenBytes: 1, reason: "keeper disconnected" },
] as const satisfies readonly AgentConversationRestoreOutcome[]) {
  test(`${outcome.status} restore is attempted once without retry or tombstone`, async () => {
    const harness = managerWithReconcileStubs(false);
    const restore = vi.fn(async () => outcome);

    await expect(reconcile(harness.manager, restore)).resolves.toMatchObject({
      admitted: true,
      resumed: 0,
      respawned: 1,
    });
    expect(harness.respawn).toHaveBeenCalledTimes(1);
    expect(restore).toHaveBeenCalledTimes(1);
    expect(restore).toHaveBeenCalledWith(
      String(SESSION_ID),
      AGENT_REFERENCE,
      expect.any(Set),
    );
    expect(harness.tombstone).not.toHaveBeenCalled();
  });
}

test("every session in one pass shares the reference claim set", async () => {
  const harness = managerWithReconcileStubs(false);
  const claimSets: Array<Set<string>> = [];
  const restore = vi.fn(async (
    _sessionId: string,
    _reference: AgentConversationReferenceV1 | null,
    resumedReferenceKeys: Set<string>,
  ): Promise<AgentConversationRestoreOutcome> => {
    claimSets.push(resumedReferenceKeys);
    resumedReferenceKeys.add("claimed");
    return { status: "accepted", writtenBytes: 1 };
  });

  await expect(reconcile(harness.manager, restore, 2)).resolves.toMatchObject({
    admitted: true,
    respawned: 2,
  });
  expect(claimSets).toHaveLength(2);
  expect(claimSets[0]).toBe(claimSets[1]);
});

test("an adopted session's reference is claimed before any respawn restore", async () => {
  const harness = managerWithReconcileStubs(
    (sessionId) => sessionId === String(SESSION_ID),
  );
  const claimed: string[][] = [];
  const restore = vi.fn(async (
    _sessionId: string,
    _reference: AgentConversationReferenceV1 | null,
    resumedReferenceKeys: Set<string>,
  ): Promise<AgentConversationRestoreOutcome> => {
    claimed.push([...resumedReferenceKeys]);
    return { status: "skipped", reason: "duplicate" };
  });

  await expect(reconcile(harness.manager, restore, 2)).resolves.toMatchObject({
    admitted: true,
    resumed: 1,
    respawned: 1,
  });
  expect(restore).toHaveBeenCalledTimes(1);
  expect(claimed[0]).toEqual([
    conversationRestoreDedupeKey(AGENT_REFERENCE),
  ]);
});

test("an unexpected restore failure cannot re-enter respawn or tombstone", async () => {
  const harness = managerWithReconcileStubs(false);
  const restore = vi.fn(async (): Promise<AgentConversationRestoreOutcome> => {
    throw new Error("restore callback failed");
  });

  await expect(reconcile(harness.manager, restore)).resolves.toMatchObject({
    admitted: true,
    respawned: 1,
  });
  expect(harness.respawn).toHaveBeenCalledTimes(1);
  expect(restore).toHaveBeenCalledTimes(1);
  expect(harness.tombstone).not.toHaveBeenCalled();
});
