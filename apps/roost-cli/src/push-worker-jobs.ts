// Post-commit worker submission for `roost push`. Each machine is an independent
// coordinator-owned durable job; deferred and failed outcomes never roll back a
// successful coordinator or another worker.

import { WorkerUpdateSource } from "@roost/shared/proto/wire_pb";
import { workerUpdateOperationFromProto } from "@roost/shared/worker-update-operation-proto";
import { buildCliContext } from "./cli-auth.ts";
import type { FleetRolloutTarget } from "./push-fleet-rollout.ts";

export interface IndependentWorkerUpdateResult {
  fingerprint: string;
  label: string;
  outcome: "updated" | "up-to-date" | "deferred" | "failed";
  action: string;
  jobId: string | null;
}

export async function submitIndependentWorkerUpdates(
  targets: readonly FleetRolloutTarget[],
  targetSha: string,
): Promise<IndependentWorkerUpdateResult[]> {
  const { client } = await buildCliContext();
  const inventory = await client.workersList({});
  const routable = new Set(inventory.routableFps);
  return Promise.all(targets.map(async (target) => {
    const worker = inventory.workers.find(candidate => candidate.fp === target.fingerprint);
    const label = worker?.label || target.host;
    if (!worker) return result(target, label, "failed", "worker is no longer registered");
    const existingOperation = worker.updateOperation
      ? workerUpdateOperationFromProto(worker.updateOperation)
      : null;
    if (worker.gitSha === targetSha
      && (existingOperation === null || existingOperation.status === "succeeded")) {
      return result(target, label, "up-to-date", "none");
    }
    const started = await client.workersDeployStart({
      host: target.fingerprint,
      expectedGitSha: targetSha,
      source: WorkerUpdateSource.PUSH,
    });
    if (!started.ok || !started.jobId) {
      return result(target, label, "failed", started.error || "update request refused");
    }
    try {
      for await (const frame of client.workersDeployOutput({ jobId: started.jobId })) {
        if (frame.kind === "operation" && frame.operation) {
          const operation = workerUpdateOperationFromProto(frame.operation);
          if (operation.status === "succeeded") {
            return result(target, label, "updated", "none", started.jobId);
          }
          if (operation.status === "failed") {
            return result(
              target,
              label,
              "failed",
              operation.message ?? "retry from Machines",
              started.jobId,
            );
          }
          if (operation.status === "blocked" || operation.status === "waiting") {
            return result(
              target,
              label,
              "deferred",
              deferredAction(operation.reasonCode, routable.has(target.fingerprint)),
              started.jobId,
            );
          }
        }
        if (frame.kind === "done") {
          return frame.exit === 0
            ? result(target, label, "updated", "none", started.jobId)
            : result(
                target,
                label,
                "failed",
                frame.error || "retry from Machines",
                started.jobId,
              );
        }
      }
      return result(target, label, "deferred", "Refresh status; report unavailable", started.jobId);
    } catch {
      return result(target, label, "deferred", "Refresh status; report unavailable", started.jobId);
    }
  }));
}

export function printIndependentWorkerUpdateResults(
  results: readonly IndependentWorkerUpdateResult[],
): void {
  const counts = {
    updated: results.filter(result => result.outcome === "updated").length,
    upToDate: results.filter(result => result.outcome === "up-to-date").length,
    deferred: results.filter(result => result.outcome === "deferred").length,
    failed: results.filter(result => result.outcome === "failed").length,
  };
  console.log(
    `>> workers updated=${counts.updated} up-to-date=${counts.upToDate}`
      + ` deferred=${counts.deferred} failed=${counts.failed}`,
  );
  for (const worker of results.filter(result =>
    result.outcome === "deferred" || result.outcome === "failed")) {
    console.log(`   ${worker.label}: ${worker.outcome}; ${worker.action}`);
  }
}

function result(
  target: FleetRolloutTarget,
  label: string,
  outcome: IndependentWorkerUpdateResult["outcome"],
  action: string,
  jobId: string | null = null,
): IndependentWorkerUpdateResult {
  return { fingerprint: target.fingerprint, label, outcome, action, jobId };
}

function deferredAction(reasonCode: string | null, routable: boolean): string {
  if (reasonCode === "offline" || !routable) {
    return "updates automatically when this machine returns; Retry is also available";
  }
  if (reasonCode === "runtime_unavailable") return "restore the installed worker runtime, then Retry";
  if (reasonCode === "keeper_incompatible" || reasonCode === "keeper_unproven") {
    return "end expendable PTYs or use explicit keeper maintenance, then Retry";
  }
  return "Retry from Machines after the reported condition changes";
}
