// Validates the coordinator journal that intentionally coexists with a local
// worker journal during atomic fleet settlement. Standalone and mismatched
// worker deploys remain blocked by the normal foreign-journal guard.

import { join, resolve } from "node:path";
import { coordServicePath } from "@roost/shared/paths";
import {
  coordinatorDeployJournalPath,
  loadCoordinatorDeployJournal,
  type CoordinatorDeployJournalContext,
} from "./coordinator-deploy-journal.ts";
import { assertWorkerRolloutDirective } from "./worker-deploy-rollout.ts";
import type { WorkerRolloutDirective } from "./worker-deploy-rollout.ts";

export function coordinatorJournalAllowsLocalWorkerRollout(
  serviceDir: string,
  platform: "darwin" | "linux",
  rawDirective: Readonly<WorkerRolloutDirective>,
  loadJournal: typeof loadCoordinatorDeployJournal = loadCoordinatorDeployJournal,
): boolean {
  const directive = assertWorkerRolloutDirective(rawDirective);
  const transactionRoot = join(serviceDir, "transactions");
  const context: CoordinatorDeployJournalContext = {
    servicePath: resolve(coordServicePath(process.env, platform)),
    releaseRoot: join(serviceDir, "releases", "coord"),
    transactionRoot,
    platform,
  };
  const journal = loadJournal(
    coordinatorDeployJournalPath(transactionRoot),
    context,
  );
  if (!journal
    || journal.rolloutId !== directive.rolloutId
    || journal.targetSha !== directive.targetSha
    || journal.priorSha !== directive.priorSha) {
    return false;
  }
  return directive.action === "finalize"
    ? journal.phase === "finalizing"
    : journal.phase === "fleet-converging";
}
