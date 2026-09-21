// Independent-update product-flow proof. Real coordinator RPC admission drives
// three durable jobs: one keeper-preserving handoff, one injected activation
// failure, and one offline operation that catches up after routability returns.

import { readFileSync, writeFileSync } from "node:fs";
import { WorkerUpdateSource } from "../../apps/shared/src/gen/roost/v1/wire_pb.ts";
import { workerUpdateOperationFromProto } from "../../apps/shared/src/worker-update-operation-proto.ts";
import type { WorkerUpdateOperation } from "../../apps/shared/src/worker-update-operation.ts";
import { buildAuthorizedApiClient } from "../../apps/roost-cli/src/api.ts";
import { REPOSITORY_ROOT, waitFor } from "../terminal/stack-runtime.ts";
import { test, expect } from "./fixtures.ts";
import type { ReleaseHandoffRequest } from "./release-handoff.ts";
import {
  keeperRuntimeOrThrow,
  openMarkedTerminals,
  waitForKeeperReconciliationAfter,
  waitForPaintedMarkers,
  workerRow,
} from "./upgrade-probes.ts";

const UPDATE_SETTLE_TIMEOUT_MS = 180_000;

test("successful, failed, and returning workers settle independently", async ({
  browser,
  install,
}, testInfo) => {
  const terminals = await openMarkedTerminals(browser, install.stack, testInfo);
  const installedWorker = workerRow(install.stack);
  const before = keeperRuntimeOrThrow(installedWorker);
  const installedWorkerPid = install.stack.workerPid();
  if (!installedWorkerPid) throw new Error("installed worker pid is unavailable");
  const handoff: ReleaseHandoffRequest = {
    workerServiceSpecPath: install.stack.workerServiceSpecPath,
    coordDbPath: install.stack.coordDbPath,
    coordinatorUrl: install.stack.baseUrl,
    apiKeyPath: install.stack.apiKeyPath,
    host: installedWorker.label,
    sourceRoot: REPOSITORY_ROOT,
    gitSha: install.workingTreeGitSha,
    installedWorkerPid,
    workerPidFilePath: `${install.stateRoot}/deployed-worker.pid`,
    forceLiveKeeperRetire: false,
  };
  writeRuntimeConfig(install, handoff, false);

  const client = await buildAuthorizedApiClient({
    coordinatorUrl: install.stack.baseUrl,
    keyPath: install.stack.apiKeyPath,
    label: "independent-updates-smoke",
  });
  const [successfulStart, failedStart, offlineStart] = await Promise.all([
    client.workersDeployStart({
      host: installedWorker.fingerprint,
      expectedGitSha: install.workingTreeGitSha,
      source: WorkerUpdateSource.PUSH,
    }),
    client.workersDeployStart({
      host: install.failedWorkerFp,
      expectedGitSha: install.workingTreeGitSha,
      source: WorkerUpdateSource.PUSH,
    }),
    client.workersDeployStart({
      host: install.offlineWorkerFp,
      expectedGitSha: install.workingTreeGitSha,
      source: WorkerUpdateSource.PUSH,
    }),
  ]);
  expect(successfulStart.ok).toBe(true);
  expect(failedStart.ok).toBe(true);
  expect(offlineStart.ok).toBe(true);

  const [successful, failed, offline] = await Promise.all([
    waitForOperation(client, successfulStart.jobId, ["succeeded"]),
    waitForOperation(client, failedStart.jobId, ["failed"]),
    waitForOperation(client, offlineStart.jobId, ["waiting"]),
  ]);
  expect(successful.status).toBe("succeeded");
  expect(failed.status).toBe("failed");
  expect(offline.reasonCode).toBe("offline");

  const after = await waitForKeeperReconciliationAfter(
    install.stack,
    before.reconciled_at_ms,
  );
  expect(after.keeper_pid).toBe(before.keeper_pid);
  expect(after.keeper_epoch).toBe(before.keeper_epoch);
  expect(after.binding_digest).toBe(before.binding_digest);
  expect(after.channel_count).toBe(before.channel_count);
  expect(workerRow(install.stack).gitSha).toBe(install.workingTreeGitSha);
  expect(readFileSync(handoff.workerPidFilePath, "utf8").trim())
    .not.toBe(String(installedWorkerPid));
  const deployedWorkerPid = Number(readFileSync(handoff.workerPidFilePath, "utf8"));
  if (!Number.isSafeInteger(deployedWorkerPid) || deployedWorkerPid <= 0) {
    throw new Error("deployed worker pid is invalid");
  }
  install.stack.adoptDeployedWorker(deployedWorkerPid);

  writeRuntimeConfig(install, handoff, true);
  await waitFor("offline worker routability", 10_000, async () => {
    const workers = await client.workersList({});
    return workers.routableFps.includes(install.offlineWorkerFp) ? true : undefined;
  });
  const retry = await client.workersDeployStart({
    host: install.offlineWorkerFp,
    expectedGitSha: install.workingTreeGitSha,
    source: WorkerUpdateSource.MANUAL,
  });
  expect(retry.jobId).toBe(offlineStart.jobId);
  const caughtUp = await waitForOperation(client, retry.jobId, ["succeeded"]);
  expect(caughtUp.status).toBe("succeeded");
  await waitForPaintedMarkers(browser, install.stack, testInfo, terminals);
});

async function waitForOperation(
  client: Awaited<ReturnType<typeof buildAuthorizedApiClient>>,
  jobId: string,
  terminalStatuses: readonly WorkerUpdateOperation["status"][],
): Promise<WorkerUpdateOperation> {
  const deadline = Date.now() + UPDATE_SETTLE_TIMEOUT_MS;
  for await (const frame of client.workersDeployOutput({ jobId })) {
    if (frame.kind !== "operation" || !frame.operation) continue;
    const operation = workerUpdateOperationFromProto(frame.operation);
    if (terminalStatuses.includes(operation.status)) return operation;
    if (Date.now() >= deadline) break;
  }
  throw new Error(`worker update ${jobId} did not reach ${terminalStatuses.join("|")}`);
}

function writeRuntimeConfig(
  install: {
    updateRuntimeConfigPath: string;
    failedWorkerFp: string;
    offlineWorkerFp: string;
  },
  handoff: ReleaseHandoffRequest,
  offlineWorkerOnline: boolean,
): void {
  writeFileSync(install.updateRuntimeConfigPath, JSON.stringify({
    failedWorkerFp: install.failedWorkerFp,
    offlineWorkerFp: install.offlineWorkerFp,
    offlineWorkerOnline,
    handoff,
  }));
}
