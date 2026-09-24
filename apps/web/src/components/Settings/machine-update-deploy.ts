// Owns "a coordinator deploy job is running for this machine" — the
// deployInFlight input of workerUpdateState. The record is module-scoped, not
// MachineCard state, because the job outlives the row that started it (details
// collapse, list re-sort, pane close) and two rows must never drive two deploys
// for one host. Callers: components/Settings/MachineCard.tsx.
// Depends on: connect.ts (coordClient), @roost/observability/diag.

import { createSignal } from "solid-js";
import { diag } from "@roost/observability/diag";
import { coordClient } from "../../connect.ts";

const [deployingFps, setDeployingFps] = createSignal<ReadonlySet<string>>(new Set());

/** Reactive: true while this machine's coordinator deploy job is running. */
export function machineDeployInFlight(fp: string): boolean {
  return deployingFps().has(fp);
}

/** Drop every in-flight record. Exists so a test can start from a clean fleet;
 *  no product path forgets a running job. */
export function _resetMachineDeploys(): void {
  setDeployingFps(new Set<string>());
}

/** Start the coordinator's deploy job for one machine and drain its output to
 *  the terminal frame. Resolves null on success, else the failure text the row
 *  shows. `expectedGitSha` pins the release the fleet is converging on — the
 *  running coordinator's own SHA, which is the desired fleet SHA. */
export async function startMachineUpdateDeploy(
  fp: string,
  expectedGitSha: string,
): Promise<string | null> {
  if (machineDeployInFlight(fp)) return null;
  markDeploying(fp, true);
  diag("machine.update.start", { worker_fp: fp, expected_git_sha: expectedGitSha });
  const failure = await drainDeployJob(fp, expectedGitSha);
  markDeploying(fp, false);
  diag(failure ? "machine.update.failed" : "machine.update.done", {
    worker_fp: fp,
    error: failure ?? "",
  });
  return failure;
}

/** The whole RPC exchange, so the in-flight record is released and logged on
 *  exactly one path whatever the coordinator or the transport does. */
async function drainDeployJob(fp: string, expectedGitSha: string): Promise<string | null> {
  try {
    const started = await coordClient.workersDeployStart({ host: fp, expectedGitSha });
    if (!started.ok || !started.jobId) {
      return started.error || "coordinator refused to start the update";
    }
    for await (const frame of coordClient.workersDeployOutput({ jobId: started.jobId })) {
      if (frame.kind !== "done") continue;
      return frame.exit === 0 ? null : frame.error || `update failed with exit ${frame.exit}`;
    }
    return "update output stream ended without a result";
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
}

function markDeploying(fp: string, running: boolean): void {
  setDeployingFps((current) => {
    const next = new Set(current);
    if (running) next.add(fp);
    else next.delete(fp);
    return next;
  });
}
