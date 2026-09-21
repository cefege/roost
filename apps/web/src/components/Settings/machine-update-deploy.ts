// Owns the browser's short-lived update-start guard and durable report reader.
// Coordinator job records remain authoritative after the request returns; this
// module never turns a client output stream into machine update state.
// Callers: MachineUpdateDetails.tsx. Depends on the typed coordinator RPC.

import { createSignal } from "solid-js";
import { diag } from "@roost/shared/diag";
import { WorkerUpdateSource } from "@roost/shared/proto/wire_pb";
import type { WorkerUpdateReport } from "@roost/shared/worker-update-operation";
import { workerUpdateReportFromProto } from "@roost/shared/worker-update-operation-proto";
import { coordClient } from "../../connect.ts";

export type MachineUpdateReportResult =
  | { readonly kind: "available"; readonly report: WorkerUpdateReport }
  | { readonly kind: "unavailable" };

const [pendingStartFps, setPendingStartFps] = createSignal<ReadonlySet<string>>(new Set());
const [reportRefreshRevision, setReportRefreshRevision] = createSignal(0);

export const machineUpdateReportRefreshRevision = reportRefreshRevision;

export function noteMachineUpdateStatusRefreshed(): void {
  setReportRefreshRevision((revision) => revision + 1);
}

/** Reactive only while this browser has not received its DeployStart response. */
export function machineUpdateStartPending(fp: string): boolean {
  return pendingStartFps().has(fp);
}

/** Test-only reset for the browser-local request guard. */
export function _resetMachineUpdateStarts(): void {
  setPendingStartFps(new Set<string>());
}

/** Request an authoritative manual update. A returned error only describes this
 * admission request; operation progress and completion arrive through workers. */
export async function startMachineUpdateDeploy(
  fp: string,
  expectedGitSha: string,
): Promise<string | null> {
  if (machineUpdateStartPending(fp)) return null;
  markStartPending(fp, true);
  diag("machine.update.start", { worker_fp: fp, expected_git_sha: expectedGitSha });
  try {
    const started = await coordClient.workersDeployStart({
      host: fp,
      expectedGitSha,
      source: WorkerUpdateSource.MANUAL,
    });
    if (!started.ok || !started.jobId) {
      const error = started.error || "Coordinator refused to start the update";
      diag("machine.update.refused", { worker_fp: fp, error });
      return error;
    }
    diag("machine.update.accepted", { worker_fp: fp, job_id: started.jobId });
    return null;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    diag("machine.update.start_failed", { worker_fp: fp, error: message });
    return message;
  } finally {
    markStartPending(fp, false);
  }
}

/** Reopen the coordinator-owned record stream for a durable report. Stream
 * transport failure is intentionally reported separately from job failure. */
export async function readMachineUpdateReport(
  jobId: string,
  signal?: AbortSignal,
): Promise<MachineUpdateReportResult> {
  try {
    for await (const frame of coordClient.workersDeployOutput({ jobId }, signal ? { signal } : undefined)) {
      if (frame.kind !== "report" || !frame.report) continue;
      return { kind: "available", report: workerUpdateReportFromProto(frame.report) };
    }
  } catch {
    if (!signal?.aborted) {
      diag("machine.update.report_unavailable", { job_id: jobId, reason: "stream_failed" });
    }
    return { kind: "unavailable" };
  }
  if (!signal?.aborted) {
    diag("machine.update.report_unavailable", { job_id: jobId, reason: "missing_report" });
  }
  return { kind: "unavailable" };
}

function markStartPending(fp: string, pending: boolean): void {
  setPendingStartFps((current) => {
    const next = new Set(current);
    if (pending) next.add(fp);
    else next.delete(fp);
    return next;
  });
}
