// Keeper-specific convergence proof for POSIX worker rollouts.
// Atomic push and coordinator recovery call this against fresh status rows.
// It proves either identity-preserving adoption or exact empty replacement.

import {
  KEEPER_EMPTY_BINDING_DIGEST,
  keeperContractsExactlyEqual,
  keeperContractsSameImplementation,
  type JournaledKeeperUpdateV1,
} from "@roost/shared/keeper-update";
import type { WorkerStatus } from "./status-types.ts";

export type KeeperUpdateConvergencePlan = JournaledKeeperUpdateV1;

export function keeperUpdateConvergenceProblem(
  worker: WorkerStatus,
  plan: KeeperUpdateConvergencePlan,
  expected: "source" | "target",
  coordinatorHeartbeatBaselineMs?: number,
  reconciliationBaselineMs?: number,
  requirePreservedIdentity = true,
  requirePreservedBindings = true,
): string | null {
  const { admission, source_contract: source, target_contract: target } = plan;
  if (worker.stale) return `${worker.label}: stale`;
  if (coordinatorHeartbeatBaselineMs !== undefined
    && worker.lastSeenMs <= coordinatorHeartbeatBaselineMs) {
    return `${worker.label}: awaiting a post-rollout heartbeat`;
  }
  const observation = worker.keeperRuntime;
  if (!observation) return `${worker.label}: keeper runtime proof is unavailable`;
  if (reconciliationBaselineMs !== undefined
    && observation.reconciled_at_ms === reconciliationBaselineMs) {
    return `${worker.label}: awaiting a post-rollout keeper reconciliation`;
  }
  if (observation.channel_count !== worker.coordinatorOpenSessionIds.length) {
    return `${worker.label}: keeper and coordinator session counts disagree`;
  }

  const expectedContract = expected === "source" ? source : target;
  if (admission.required_action === "preserve") {
    if (!keeperContractsSameImplementation(
      expectedContract,
      observation.running_contract,
    )) {
      return `${worker.label}: preserved keeper implementation changed`;
    }
    if (requirePreservedIdentity
      && (observation.keeper_pid !== admission.expected_keeper_pid
        || observation.keeper_epoch !== admission.expected_keeper_epoch)) {
      return `${worker.label}: preserved keeper identity changed`;
    }
    if (requirePreservedBindings
      && observation.binding_digest !== admission.expected_binding_digest) {
      return `${worker.label}: preserved keeper bindings changed`;
    }
    return null;
  }

  if (expected === "target"
    && observation.keeper_epoch === admission.expected_keeper_epoch) {
    return `${worker.label}: empty keeper replacement did not advance its epoch`;
  }
  if (expected === "source"
    && observation.keeper_epoch === admission.expected_keeper_epoch
    && observation.keeper_pid !== admission.expected_keeper_pid) {
    return `${worker.label}: original source keeper PID changed`;
  }
  if (observation.channel_count !== 0
    || worker.coordinatorOpenSessionIds.length !== 0
    || observation.binding_digest !== KEEPER_EMPTY_BINDING_DIGEST) {
    return `${worker.label}: replacement keeper bindings are not proven empty`;
  }
  const contractMatches = expected === "source"
    ? keeperContractsSameImplementation(expectedContract, observation.running_contract)
    : keeperContractsExactlyEqual(expectedContract, observation.running_contract);
  if (!contractMatches) {
    return `${worker.label}: replacement keeper is not the ${expected} implementation`;
  }
  return null;
}
