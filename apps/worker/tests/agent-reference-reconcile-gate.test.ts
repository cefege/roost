// Proves reference reports serialize behind complete boot reconciliation,
// not merely the coordinator recovery read. A reporter may replace recovery
// state only after adoption, respawn, and optional restore have settled.

import { afterEach, expect, test, vi } from "bun:test";
import { asWorkerFp } from "@roost/shared/wire";
import type { CoordClient } from "../src/coord-client.ts";
import { AgentReferenceAdmissionGate } from "../src/agent-status/reference-admission.ts";
import { setupReconcile } from "../src/boot-reconcile.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import { SessionManager } from "../src/session-manager.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";

const managers: SessionManager[] = [];

afterEach(() => {
  for (const manager of managers.splice(0).reverse()) manager.dispose();
  getMultiplexedPool()._onKeeperDeath = null;
  vi.restoreAllMocks();
});

test("reference admission remains held through post-read reconciliation", async () => {
  const referenceAdmission = new AgentReferenceAdmissionGate();
  const manager = new SessionManager({
    workerFp: asWorkerFp("53".repeat(32)),
    sink: new SessionEventTestSink(),
  });
  managers.push(manager);
  manager.startPostAdmissionMaintenance = vi.fn(async () => {});
  manager.advanceChannelCounterPastKeeper = vi.fn(async () => {});
  manager.reapStrayKeeperChannels = vi.fn(async () => 0);
  const preparationStarted = Promise.withResolvers<void>();
  const allowPreparation = Promise.withResolvers<void>();
  const { reconcileOpenSessions } = setupReconcile({
    client: () => ({
      sessionsList: async () => ({ sessions: [], recoveryMetadata: [] }),
    }) as unknown as CoordClient,
    workerFp: manager.workerFp,
    sessionMgr: manager,
    referenceAdmission,
    beforeRecoveryRead: async () => {},
    prepareKeeper: async () => {
      preparationStarted.resolve();
      await allowPreparation.promise;
    },
    restoreAgentConversation: async () => ({
      status: "skipped",
      reason: "missing_reference",
    }),
  });

  const reconciliation = reconcileOpenSessions("boot");
  await preparationStarted.promise;
  let reporterEntered = false;
  const reporter = referenceAdmission.runExclusive(async () => {
    reporterEntered = true;
  });
  await Promise.resolve();
  expect(reporterEntered).toBe(false);

  allowPreparation.resolve();
  await expect(reconciliation).resolves.toMatchObject({ admitted: true });
  await reporter;
  expect(reporterEntered).toBe(true);
});
