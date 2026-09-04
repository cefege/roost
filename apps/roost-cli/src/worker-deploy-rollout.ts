// Shared identity and action contract for workers participating in one atomic
// `roost push` rollout. The platform journals persist the ID; deploy drivers
// use the action to hold, finalize, or roll back an exact target build.

import { POSIX_FULL_GIT_SHA_RE } from "./posix-deploy-journal.ts";

const WORKER_ROLLOUT_ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export type WorkerRolloutAction = "hold" | "finalize" | "rollback";

export interface WorkerRolloutDirective {
  action: WorkerRolloutAction;
  rolloutId: string;
  targetSha: string;
  priorSha: string;
}

export function assertWorkerRolloutId(value: unknown, label = "worker rollout ID"): string {
  if (typeof value !== "string" || !WORKER_ROLLOUT_ID_RE.test(value)) {
    throw new Error(`${label} must be a canonical UUID`);
  }
  return value.toLowerCase();
}

export function workerRolloutIdOrNull(value: unknown, label = "worker rollout ID"): string | null {
  if (value === undefined || value === null || value === "") return null;
  return assertWorkerRolloutId(value, label);
}

export function assertWorkerRolloutDirective(
  directive: Readonly<WorkerRolloutDirective>,
): WorkerRolloutDirective {
  if (directive.action !== "hold"
    && directive.action !== "finalize"
    && directive.action !== "rollback") {
    throw new Error("worker rollout action is invalid");
  }
  if (!POSIX_FULL_GIT_SHA_RE.test(directive.targetSha)
    || !POSIX_FULL_GIT_SHA_RE.test(directive.priorSha)) {
    throw new Error("worker rollout SHA identities must be full hexadecimal object IDs");
  }
  return {
    action: directive.action,
    rolloutId: assertWorkerRolloutId(directive.rolloutId),
    targetSha: directive.targetSha.toLowerCase(),
    priorSha: directive.priorSha.toLowerCase(),
  };
}

export function assertWorkerRolloutMatches(
  journal: { rolloutId: string | null; targetSha: string },
  directive: Readonly<WorkerRolloutDirective>,
): void {
  const checked = assertWorkerRolloutDirective(directive);
  if (journal.rolloutId !== checked.rolloutId || journal.targetSha.toLowerCase() !== checked.targetSha) {
    throw new Error("worker deploy journal does not match the requested fleet rollout");
  }
}
