// Shared POSIX direct-deploy keeper admission and coordinator preparation.
// Deployment journals retain the immutable envelope and exact worker identity;
// the coordinator owns the final channel-creation drain and keeper mutation.

import {
  JournaledKeeperUpdateV1Schema,
  keeperUpdateAdmission,
  keeperUpdateOutcomeMatchesAction,
  type JournaledKeeperUpdateV1,
  type KeeperContractV1,
} from "@roost/shared/keeper-update";
import { buildDashboardScopedCliContext } from "./cli-auth.ts";
import { normalizedHost } from "./deploy-windows-channel.ts";
import { _isSelfHost } from "./deploy-self-host.ts";
import type { DirectKeeperAdmissionOutcome } from "./keeper-admission-staging.ts";
import { keeperUpdateConvergenceProblem } from "./keeper-update-convergence.ts";
import {
  routableWorkerFingerprints,
  workerInventoryForUpdateAdmission,
  type WorkerStatus,
} from "./status.ts";

interface KeeperPreparationRequest {
  workerFp: string;
  journaledUpdateJson: string;
  direction: "source" | "target";
  maintenance: false;
}

export interface JournaledKeeperUpdateCallbackRuntime {
  prepare: (request: KeeperPreparationRequest) => Promise<{
    outcome: string;
    keeperPid?: bigint;
    keeperEpoch?: string;
    bindingDigest?: string;
  }>;
  inventory: () => readonly WorkerStatus[];
  routable: (workerFingerprint: string) => Promise<boolean>;
  sleep: (milliseconds: number) => Promise<void>;
  attempts: number;
}

export interface JournaledKeeperUpdateCallbacks {
  apply(
    workerFingerprint: string,
    update: Readonly<JournaledKeeperUpdateV1>,
    direction: "source" | "target",
  ): Promise<void>;
  prove(
    workerFingerprint: string,
    update: Readonly<JournaledKeeperUpdateV1>,
    direction: "source" | "target",
    expectedWorkerSha: string,
    heartbeatNotBeforeMs: number,
  ): Promise<void>;
}

function workerMatchesTarget(worker: WorkerStatus, host: string): boolean {
  const expected = normalizedHost(host);
  return normalizedHost(worker.fingerprint) === expected
    || normalizedHost(worker.label) === expected
    || normalizedHost(worker.reachableAddr ?? "") === expected;
}

export function workerForDirectKeeperTarget(
  host: string,
  inventory: readonly WorkerStatus[] = workerInventoryForUpdateAdmission(),
): WorkerStatus {
  const matches = inventory.filter(worker => workerMatchesTarget(worker, host));
  if (matches.length !== 1) {
    throw new Error(`${host}: update action cannot resolve exactly one worker`);
  }
  return matches[0]!;
}

async function matchingLocalWorkers(
  inventory: readonly WorkerStatus[],
  isSelfHost: (host: string) => Promise<boolean>,
): Promise<WorkerStatus[]> {
  const matches: WorkerStatus[] = [];
  for (const worker of inventory) {
    if (await isSelfHost(worker.reachableAddr || worker.label)) matches.push(worker);
  }
  return matches;
}

function requireUniqueLocalWorker(matches: readonly WorkerStatus[]): WorkerStatus {
  if (matches.length !== 1) {
    throw new Error("self-update admission cannot resolve exactly one local worker");
  }
  return matches[0]!;
}

export async function localUpdateWorker(
  inventory: readonly WorkerStatus[] = workerInventoryForUpdateAdmission(),
  isSelfHost: (host: string) => Promise<boolean> = _isSelfHost,
): Promise<WorkerStatus> {
  const worker = requireUniqueLocalWorker(
    await matchingLocalWorkers(inventory, isSelfHost),
  );
  // Keeper maintenance destroys PTYs, so it demands proof the coordinator has
  // refreshed recently. Deploy admission classifies staleness instead of
  // throwing, because a host with no worker service has nothing to protect.
  if (worker.stale) throw new Error("self-update keeper runtime proof is stale");
  return worker;
}

export async function localUpdateWorkerForAdmission(
  bootstrapAllowed: boolean,
  inventory?: readonly WorkerStatus[],
  isSelfHost: (host: string) => Promise<boolean> = _isSelfHost,
): Promise<WorkerStatus | null> {
  let available: readonly WorkerStatus[];
  try {
    available = inventory ?? workerInventoryForUpdateAdmission();
  } catch (error) {
    if (bootstrapAllowed) return null;
    throw error;
  }
  const matches = await matchingLocalWorkers(available, isSelfHost);
  if (matches.length === 0 && bootstrapAllowed) return null;
  return requireUniqueLocalWorker(matches);
}

export function directKeeperUpdateAdmission(
  host: string,
  targetContract: KeeperContractV1,
  bootstrapAllowed: boolean,
  inventory?: readonly WorkerStatus[],
): DirectKeeperAdmissionOutcome {
  let available: readonly WorkerStatus[];
  try {
    available = inventory ?? workerInventoryForUpdateAdmission();
  } catch (error) {
    if (bootstrapAllowed) return { outcome: "unregistered" };
    throw error;
  }
  const matches = available.filter(worker => workerMatchesTarget(worker, host));
  if (matches.length === 0 && bootstrapAllowed) return { outcome: "unregistered" };
  if (matches.length !== 1) {
    throw new Error(`${host}: update admission cannot resolve exactly one worker`);
  }
  const worker = matches[0]!;
  // A row the coordinator has not heard from can neither prove nor refresh
  // anything, and no keeper action could reach a disconnected worker anyway.
  if (worker.stale) return { outcome: "proof-stale", workerLabel: worker.label };
  // No observation at all is the one unprovable case: the running build
  // predates keeper-runtime reporting, so it can never earn admission for the
  // update that teaches it to report. A contradicted or refused observation
  // still fails closed below.
  if (!worker.keeperRuntime) {
    return { outcome: "runtime-unreported", workerLabel: worker.label };
  }
  const admission = keeperUpdateAdmission(
    targetContract,
    worker.keeperRuntime,
    new Set(worker.coordinatorOpenSessionIds),
  );
  if (!admission) {
    throw new Error(`${worker.label}: keeper update is blocked or unproven`);
  }
  return {
    outcome: "admitted",
    workerFingerprint: worker.fingerprint,
    keeperUpdate: JournaledKeeperUpdateV1Schema.parse({
      admission,
      source_contract: worker.keeperRuntime.running_contract,
      target_contract: targetContract,
    }),
  };
}

function defaultJournaledKeeperUpdateRuntime(): JournaledKeeperUpdateCallbackRuntime {
  return {
    prepare: async request => {
      const { client } = await buildDashboardScopedCliContext();
      return await client.workersPrepareKeeperUpdate(request);
    },
    inventory: workerInventoryForUpdateAdmission,
    routable: async workerFingerprint =>
      (await routableWorkerFingerprints()).has(workerFingerprint),
    sleep: Bun.sleep,
    attempts: 60,
  };
}

export function createJournaledKeeperUpdateCallbacks(
  runtime: Readonly<JournaledKeeperUpdateCallbackRuntime> =
    defaultJournaledKeeperUpdateRuntime(),
): JournaledKeeperUpdateCallbacks {
  const attempts = Math.max(1, runtime.attempts);
  const reconciliationBaselines = new Map<string, {
    reconciledAtMs: number;
    coordinatorLastSeenMs: number;
    keeperPid: number;
    keeperEpoch: string;
    bindingDigest: string;
  }>();
  return {
    async apply(workerFingerprint, rawUpdate, direction) {
      const update = JournaledKeeperUpdateV1Schema.parse(rawUpdate);
      let lastError: unknown = new Error("keeper update RPC was not attempted");
      for (let attempt = 0; attempt < attempts; attempt += 1) {
        try {
          if (!await runtime.routable(workerFingerprint)) {
            throw new Error("worker is not coordinator-routable");
          }
          const baselineKey = `${workerFingerprint}:${direction}`;
          const matches = runtime.inventory().filter(
            worker => worker.fingerprint === workerFingerprint,
          );
          const worker = matches.length === 1 ? matches[0] : undefined;
          const reconciledAt = worker?.keeperRuntime?.reconciled_at_ms;
          if (reconciledAt === undefined || !worker) {
            throw new Error("worker reconciliation baseline is unavailable");
          }
          const response = await runtime.prepare({
            workerFp: workerFingerprint,
            journaledUpdateJson: JSON.stringify(update),
            direction,
            maintenance: false,
          });
          const action = update.admission.required_action;
          if (!keeperUpdateOutcomeMatchesAction(action, response.outcome)) {
            throw new Error(
              `worker returned outcome ${response.outcome || "<empty>"} for ${action} keeper action`,
            );
          }
          const keeperPid = response.keeperPid === undefined
            ? update.admission.expected_keeper_pid
            : Number(response.keeperPid);
          const keeperEpoch = response.keeperEpoch
            ?? update.admission.expected_keeper_epoch;
          const bindingDigest = response.bindingDigest
            ?? update.admission.expected_binding_digest;
          if (!Number.isSafeInteger(keeperPid) || keeperPid <= 0
            || !/^[0-9a-f]{64}$/.test(bindingDigest)) {
            throw new Error("worker returned malformed keeper identity");
          }
          reconciliationBaselines.set(baselineKey, {
            reconciledAtMs: reconciledAt,
            coordinatorLastSeenMs: worker.lastSeenMs,
            keeperPid,
            keeperEpoch,
            bindingDigest,
          });
          return;
        } catch (error) {
          lastError = error;
          if (attempt + 1 < attempts) await runtime.sleep(1_000);
        }
      }
      throw lastError;
    },
    async prove(
      workerFingerprint,
      update,
      direction,
      expectedWorkerSha,
      _heartbeatNotBeforeMs,
    ) {
      let problem = "worker keeper convergence was not observed";
      const baselineKey = `${workerFingerprint}:${direction}`;
      const baseline = reconciliationBaselines.get(baselineKey);
      for (let attempt = 0; attempt < attempts; attempt += 1) {
        try {
          const matches = runtime.inventory().filter(
            worker => worker.fingerprint === workerFingerprint,
          );
          if (matches.length !== 1) {
            problem = "update proof cannot resolve exactly one worker";
          } else {
            const worker = matches[0]!;
            problem = baseline === undefined
              ? `${worker.label}: worker reconciliation baseline is unavailable`
              : worker.keeperRuntime?.reconciled_at_ms === baseline.reconciledAtMs
                ? `${worker.label}: awaiting restarted worker reconciliation`
                : worker.lastSeenMs <= baseline.coordinatorLastSeenMs
                  ? `${worker.label}: awaiting restarted worker heartbeat`
                  : worker.gitSha?.toLowerCase() !== expectedWorkerSha.toLowerCase()
                    ? `${worker.label}: worker build does not match ${direction}`
                    : !await runtime.routable(workerFingerprint)
                      ? `${worker.label}: worker is not coordinator-routable`
                      : keeperUpdateConvergenceProblem(
                          worker,
                          update.admission.required_action === "preserve"
                            ? {
                                ...update,
                                admission: {
                                  ...update.admission,
                                  expected_keeper_pid: baseline.keeperPid,
                                  expected_keeper_epoch: baseline.keeperEpoch,
                                  expected_binding_digest: baseline.bindingDigest,
                                },
                              }
                            : update,
                          direction,
                          baseline.coordinatorLastSeenMs,
                          baseline.reconciledAtMs,
                        ) ?? "";
            if (problem === "") {
              reconciliationBaselines.delete(baselineKey);
              return;
            }
          }
        } catch (error) {
          problem = error instanceof Error ? error.message : String(error);
        }
        if (attempt + 1 < attempts) await runtime.sleep(1_000);
      }
      throw new Error(`keeper update convergence proof failed: ${problem}`);
    },
  };
}



/** `forceLive` is the operator's authorization to end live PTYs; it is always
 * passed explicitly so no other request field can imply it. */
export async function prepareKeeperMaintenance(
  workerFingerprint: string,
  forceLive: boolean,
): Promise<string> {
  const { client } = await buildDashboardScopedCliContext();
  const response = await client.workersPrepareKeeperUpdate({
    workerFp: workerFingerprint,
    direction: "",
    maintenance: true,
    forceLive,
  });
  if (!keeperUpdateOutcomeMatchesAction("maintenance", response.outcome)) {
    throw new Error(`worker returned unknown keeper maintenance outcome: ${response.outcome}`);
  }
  return response.outcome;
}
