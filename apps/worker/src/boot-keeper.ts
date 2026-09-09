// Keeper survivor admission for worker boot. Authenticated protocol-compatible
// survivors are adopted; automatic replacement is identity-fenced and allowed
// only after coordinator sessions and keeper channel bindings prove empty.
// An identity this probe cannot prove is reported as unproven, never as busy.
// A survivor whose Hello predates binding proof can only be retired under an
// explicit operator force-live authorization, which ends every PTY it hosts.

import {
  cleanupLocalEndpoint,
  type LocalEndpoint,
} from "@roost/shared/local-endpoint";
import { log } from "@roost/shared/log";
import { KEEPER_EMPTY_BINDING_DIGEST } from "@roost/shared/keeper-update";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import {
  probeKeeperCompatible,
  shutdownEmptyKeeperAuthenticated,
  shutdownKeeperAuthenticated,
  waitForKeeperExit,
  type KeeperProbeResult,
} from "./keeper/keeper-probe.ts";
import { muxLocalEndpoint } from "./keeper/keeper-pool-config.ts";
import { KEEPER_TARGET_CONTRACT } from "./keeper/keeper-stamp.ts";
import {
  isTerminalCoreCapacityError,
  type TerminalCoreCapacity,
} from "./terminal-core-capacity.ts";

/** A Hello that is slow is not a claim about occupancy. Retry identity to this
 * deadline so one timeout can never be reported as live sessions, and never
 * crash-loop the worker over a keeper that is merely busy. */
const KEEPER_IDENTITY_DEADLINE_MS = 5_000;
const KEEPER_IDENTITY_ATTEMPT_TIMEOUT_MS = 2_000;
const KEEPER_IDENTITY_RETRY_INTERVAL_MS = 50;

export const KEEPER_REPLACEMENT_BLOCKED_ERROR =
  "keeper replacement blocked by live sessions";
export const KEEPER_IDENTITY_UNPROVEN_ERROR =
  "keeper endpoint is held by a process that did not prove keeper identity; "
  + "stop that process, then restart the worker";
export const KEEPER_FORCE_LIVE_RETIRE_REJECTED_ERROR =
  "authenticated force-live keeper retirement was rejected";

/** A survivor that authenticates but cannot describe its channel bindings
 * predates the binding-bearing Hello. The worker can neither adopt it (wrong
 * protocol) nor prove it empty enough to replace automatically, so it is only
 * ever retired under an explicit operator authorization. */
function keeperPredatesBindingProof(probe: KeeperProbeResult): boolean {
  return probe.authenticated
    && !probe.protocolCompatible
    && (probe.bindings === undefined || probe.spawningChannels === undefined);
}

/** Retry until the survivor proves its identity, the endpoint proves absent, or
 * the deadline passes. Authentication is the only fact worth waiting for; a
 * refused connection and an authenticated Hello are both already decisive. */
async function proveKeeperSurvivorIdentity(
  endpoint: LocalEndpoint,
): Promise<KeeperProbeResult> {
  const deadline = Date.now() + KEEPER_IDENTITY_DEADLINE_MS;
  for (;;) {
    const probe = await probeKeeperCompatible(
      endpoint,
      KEEPER_IDENTITY_ATTEMPT_TIMEOUT_MS,
    );
    if (!probe.reachable || probe.authenticated || Date.now() >= deadline) {
      return probe;
    }
    log.warn("worker", "keeper_survivor_identity_retry", {
      endpoint: endpoint.address,
      kind: endpoint.kind,
    });
    await Bun.sleep(KEEPER_IDENTITY_RETRY_INTERVAL_MS);
  }
}

/** `forceLiveRetire` is the operator's authorization to discard a keeper that
 * predates binding proof; it defaults off so no boot can imply it. */
export async function handleKeeperSurvivor(
  coordinatorOpenSessionIds: ReadonlySet<string>,
  forceLiveRetire = false,
  terminalCoreCapacity?: Pick<
    TerminalCoreCapacity,
    "assertCanAdoptSurvivors" | "refuseUnknownSurvivorInventory" | "snapshot"
  >,
): Promise<void> {
  const endpoint = muxLocalEndpoint();
  const probe = await proveKeeperSurvivorIdentity(endpoint);
  if (!probe.reachable) {
    await cleanupLocalEndpoint(endpoint);
    return;
  }

  if (probe.authenticated && probe.protocolCompatible) {
    if (terminalCoreCapacity) {
      const survivorChannelCount = probe.bindings === undefined
        || probe.spawningChannels === undefined
        ? null
        : new Set([
          ...probe.bindings.map((binding) => binding.channel_id),
          ...probe.spawningChannels,
        ]).size;
      try {
        if (survivorChannelCount === null) {
          terminalCoreCapacity.refuseUnknownSurvivorInventory();
        }
        terminalCoreCapacity.assertCanAdoptSurvivors(survivorChannelCount);
      } catch (error) {
        if (isTerminalCoreCapacityError(error)) {
          const snapshot = terminalCoreCapacity.snapshot();
          log.error("worker", "keeper_survivor_capacity_refused", {
            endpoint: endpoint.address,
            kind: endpoint.kind,
            keeper_pid: probe.keeperPid ?? null,
            survivor_channels: survivorChannelCount,
            capacity: snapshot.capacity,
            used: snapshot.used,
            pending: snapshot.pending,
            refusal_count: snapshot.refusalCount,
          });
        }
        throw error;
      }
    }
    if (probe.contract) {
      getMultiplexedPool().setRunningKeeperContract(probe.contract);
    }
    log.info("worker", "keeper_survivor_adopted", {
      endpoint: endpoint.address,
      kind: endpoint.kind,
      keeper_pid: probe.keeperPid ?? null,
      process_epoch: probe.processEpoch ?? null,
      exact_target: probe.exactTarget,
      protocol_compatible: probe.protocolCompatible,
      keeper_digest: probe.contract?.implementation_digest ?? null,
      target_digest: KEEPER_TARGET_CONTRACT.implementation_digest,
      bindings: probe.bindings?.length ?? null,
    });
    return;
  }

  if (forceLiveRetire && keeperPredatesBindingProof(probe)) {
    // Emitted BEFORE the shutdown request: this line is the only record of what
    // the retirement ends. A null channel list is the survivor's own failure to
    // enumerate its bindings, which is why the authorization was required.
    log.warn("worker", "keeper_force_live_retire_discarding", {
      endpoint: endpoint.address,
      kind: endpoint.kind,
      keeper_pid: probe.keeperPid ?? null,
      process_epoch: probe.processEpoch ?? null,
      keeper_features: probe.features,
      protocol_compatible: probe.protocolCompatible,
      keeper_binding_channel_ids: probe.bindings?.map((binding) => binding.channel_id) ?? null,
      spawning_channels: probe.spawningChannels ?? null,
      coordinator_sessions: coordinatorOpenSessionIds.size,
      coordinator_session_ids: [...coordinatorOpenSessionIds],
    });
    const retired = await shutdownKeeperAuthenticated(endpoint);
    if (!retired) throw new Error(KEEPER_FORCE_LIVE_RETIRE_REJECTED_ERROR);
    if (!await waitForKeeperExit(endpoint)) {
      throw new Error("force-live retired keeper did not shut down");
    }
    await cleanupLocalEndpoint(endpoint);
    log.warn("worker", "keeper_force_live_retired", {
      endpoint: endpoint.address,
      kind: endpoint.kind,
      keeper_pid: probe.keeperPid ?? null,
      discarded_sessions: coordinatorOpenSessionIds.size,
    });
    return;
  }

  if (!probe.authenticated
    || probe.keeperPid === undefined
    || probe.processEpoch === undefined
    || probe.bindings === undefined
    || probe.spawningChannels === undefined) {
    log.error("worker", "keeper_survivor_identity_unproven", {
      endpoint: endpoint.address,
      kind: endpoint.kind,
      authenticated: probe.authenticated,
      identity_deadline_ms: KEEPER_IDENTITY_DEADLINE_MS,
    });
    throw new Error(KEEPER_IDENTITY_UNPROVEN_ERROR);
  }

  if (coordinatorOpenSessionIds.size > 0
    || probe.bindings.length !== 0
    || probe.spawningChannels.length !== 0) {
    log.warn("worker", "keeper_survivor_replacement_blocked", {
      endpoint: endpoint.address,
      kind: endpoint.kind,
      protocol_compatible: probe.protocolCompatible,
      exact_target: probe.exactTarget,
      coordinator_sessions: coordinatorOpenSessionIds.size,
      keeper_bindings: probe.bindings.length,
      spawning_channels: probe.spawningChannels.length,
      keeper_pid: probe.keeperPid,
      process_epoch: probe.processEpoch,
    });
    throw new Error(KEEPER_REPLACEMENT_BLOCKED_ERROR);
  }

  log.info("worker", "keeper_survivor_replacing_empty", {
    endpoint: endpoint.address,
    kind: endpoint.kind,
    keeper_pid: probe.keeperPid,
    process_epoch: probe.processEpoch,
    keeper_digest: probe.contract?.implementation_digest ?? null,
    target_digest: KEEPER_TARGET_CONTRACT.implementation_digest,
  });
  const stopped = await shutdownEmptyKeeperAuthenticated(endpoint, {
    keeperPid: probe.keeperPid,
    processEpoch: probe.processEpoch,
    bindingDigest: KEEPER_EMPTY_BINDING_DIGEST,
  });
  if (!stopped) {
    throw new Error(KEEPER_REPLACEMENT_BLOCKED_ERROR);
  }
  if (!await waitForKeeperExit(endpoint)) {
    throw new Error("authenticated incompatible keeper did not shut down");
  }
  await cleanupLocalEndpoint(endpoint);
}
