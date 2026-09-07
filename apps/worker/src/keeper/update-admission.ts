// Applies one immutable keeper update action at the live worker boundary.
// Coordinator preparation serializes channel admission before calling here.
// Authenticated probes fence identity/bindings; only replace-empty and an
// explicitly operator-forced maintenance refresh may shut the keeper down.

import { createHash } from "node:crypto";
import { z } from "zod";
import { log } from "@roost/shared/log";
import {
  JournaledKeeperUpdateV1Schema,
  KEEPER_EMPTY_BINDING_DIGEST,
  KeeperCoordinatorOpenSessionIdsSchema,
  keeperBindingDigestInput,
  keeperContractsExactlyEqual,
  keeperContractsSameImplementation,
  type JournaledKeeperUpdateV1,
  type KeeperUpdateOutcome,
} from "@roost/shared/keeper-update";
import { muxLocalEndpoint } from "./keeper-pool-config.ts";
import {
  probeKeeperCompatible,
  shutdownEmptyKeeperAuthenticated,
  shutdownKeeperAuthenticated,
  waitForKeeperExit,
  type KeeperProbeResult,
} from "./keeper-probe.ts";

const WorkerOpenChannelIdsSchema = z.array(
  z.number().int().positive().max(0x7fff_ffff),
).max(65_535).refine(
  channelIds => channelIds.every((channelId, index) =>
    index === 0 || channelIds[index - 1]! < channelId),
  "worker channel IDs must be sorted and unique",
).readonly();

export const JournaledKeeperUpdateActionV1Schema = z.object({
  schema_version: z.literal(1),
  update: JournaledKeeperUpdateV1Schema,
  direction: z.enum(["source", "target"]),
  coordinator_open_session_ids: KeeperCoordinatorOpenSessionIdsSchema,
  worker_open_channel_ids: WorkerOpenChannelIdsSchema,
}).strict().readonly();
export type JournaledKeeperUpdateActionV1 = z.infer<
  typeof JournaledKeeperUpdateActionV1Schema
>;
export type JournaledKeeperUpdateOutcome = KeeperUpdateOutcome;
export interface JournaledKeeperUpdateActionResult {
  outcome: KeeperUpdateOutcome;
  keeper_pid?: number;
  keeper_epoch?: string;
  binding_digest?: string;
}

interface KeeperUpdateActionDeps {
  probe?: typeof probeKeeperCompatible;
  shutdownEmpty?: typeof shutdownEmptyKeeperAuthenticated;
  shutdownForced?: typeof shutdownKeeperAuthenticated;
  sleep?: (milliseconds: number) => Promise<void>;
  now?: () => number;
}

/** Operator-authorized destruction of live channels. The journaled update
 * envelope cannot carry this: its schema is strict and has no such field, so a
 * replayed or hand-edited journal is rejected rather than granted the flag. */
export interface KeeperMaintenanceRequest {
  forceLive: boolean;
}

function bindingDigest(probe: KeeperProbeResult): string | null {
  if (!probe.bindings || !probe.spawningChannels) return null;
  return createHash("sha256").update(keeperBindingDigestInput(
    probe.bindings,
    probe.spawningChannels,
  )).digest("hex");
}

function requireAuthenticatedProof(probe: KeeperProbeResult): asserts probe is KeeperProbeResult & {
  authenticated: true;
  contract: NonNullable<KeeperProbeResult["contract"]>;
  keeperPid: number;
  processEpoch: string;
  bindings: NonNullable<KeeperProbeResult["bindings"]>;
  spawningChannels: NonNullable<KeeperProbeResult["spawningChannels"]>;
} {
  if (!probe.authenticated || !probe.contract || probe.keeperPid === undefined
    || !probe.processEpoch || !probe.bindings || !probe.spawningChannels) {
    throw new Error("keeper identity is unproven: no authenticated runtime proof");
  }
}

async function requireKeeperExit(
  probe: typeof probeKeeperCompatible,
  sleep: (milliseconds: number) => Promise<void>,
  now: () => number,
): Promise<void> {
  const exited = await waitForKeeperExit(muxLocalEndpoint(), { probe, sleep, now });
  if (!exited) throw new Error("authenticated keeper did not exit");
}

export async function applyJournaledKeeperUpdateAction(
  value: JournaledKeeperUpdateActionV1,
  deps: KeeperUpdateActionDeps = {},
): Promise<JournaledKeeperUpdateActionResult> {
  const action = JournaledKeeperUpdateActionV1Schema.parse(value);
  const probe = deps.probe ?? probeKeeperCompatible;
  const shutdownEmpty = deps.shutdownEmpty ?? shutdownEmptyKeeperAuthenticated;
  const sleep = deps.sleep ?? Bun.sleep;
  const now = deps.now ?? Date.now;
  const endpoint = muxLocalEndpoint();
  const current = await probe(endpoint);
  const { admission, source_contract: source, target_contract: target } = action.update;
  const desired = action.direction === "target" ? target : source;
  if (admission.required_action === "replace-empty"
    && action.coordinator_open_session_ids.length !== 0) {
    throw new Error("replace-empty action is blocked by live sessions");
  }

  if (!current.reachable) {
    if (admission.required_action === "replace-empty") {
      return { outcome: "already-absent" };
    }
    throw new Error("preserve action cannot prove a running keeper");
  }
  requireAuthenticatedProof(current);
  const currentDigest = bindingDigest(current)!;
  const keeperOpenChannelIds = [
    ...current.bindings.map(binding => binding.channel_id),
    ...current.spawningChannels,
  ].sort((left, right) => left - right);
  if (keeperOpenChannelIds.length !== action.worker_open_channel_ids.length
    || keeperOpenChannelIds.some(
      (channelId, index) => channelId !== action.worker_open_channel_ids[index],
    )) {
    throw new Error("worker sessions and keeper channels changed after admission");
  }
  if (action.worker_open_channel_ids.length
    !== action.coordinator_open_session_ids.length) {
    throw new Error("coordinator sessions and worker sessions changed after admission");
  }

  if (admission.required_action === "preserve") {
    if (!keeperContractsSameImplementation(desired, current.contract)
      || (action.direction === "target"
        && (current.keeperPid !== admission.expected_keeper_pid
          || current.processEpoch !== admission.expected_keeper_epoch))) {
      throw new Error("journaled preserve identity no longer matches the keeper");
    }
    log.info("keeper-update", "keeper_preserved", {
      keeper_pid: current.keeperPid,
      keeper_epoch: current.processEpoch,
      binding_digest: currentDigest,
    });
    return {
      outcome: "preserved",
      keeper_pid: current.keeperPid,
      keeper_epoch: current.processEpoch,
      binding_digest: currentDigest,
    };
  }

  if (keeperOpenChannelIds.length !== 0
    || currentDigest !== KEEPER_EMPTY_BINDING_DIGEST) {
    throw new Error("replace-empty action is blocked by live sessions");
  }
  const desiredMatches = action.direction === "source"
    ? keeperContractsSameImplementation(desired, current.contract)
    : keeperContractsExactlyEqual(desired, current.contract);
  if (desiredMatches) {
    const originalIdentity = current.keeperPid === admission.expected_keeper_pid
      && current.processEpoch === admission.expected_keeper_epoch;
    if (action.direction === "source" || !originalIdentity) {
      return { outcome: "already-converged" };
    }
  }

  const replaceable = action.direction === "target" ? source : target;
  if (!keeperContractsSameImplementation(replaceable, current.contract)) {
    throw new Error("empty keeper does not match the journaled replace source");
  }
  if (action.direction === "target"
    && (current.keeperPid !== admission.expected_keeper_pid
      || current.processEpoch !== admission.expected_keeper_epoch
      || currentDigest !== admission.expected_binding_digest)) {
    throw new Error("forward empty replacement lost its admitted keeper identity");
  }
  const stopped = await shutdownEmpty(endpoint, {
    keeperPid: current.keeperPid,
    processEpoch: current.processEpoch,
    bindingDigest: currentDigest,
  });
  if (!stopped) throw new Error("authenticated empty keeper shutdown was rejected");
  await requireKeeperExit(probe, sleep, now);
  log.info("keeper-update", "empty_keeper_shutdown", {
    direction: action.direction,
    keeper_pid: current.keeperPid,
    keeper_epoch: current.processEpoch,
  });
  return { outcome: "shutdown" };
}

export async function shutdownKeeperForMaintenance(
  request: KeeperMaintenanceRequest,
  deps: KeeperUpdateActionDeps = {},
): Promise<"shutdown" | "already-absent"> {
  const probe = deps.probe ?? probeKeeperCompatible;
  const shutdownEmpty = deps.shutdownEmpty ?? shutdownEmptyKeeperAuthenticated;
  const shutdownForced = deps.shutdownForced ?? shutdownKeeperAuthenticated;
  const sleep = deps.sleep ?? Bun.sleep;
  const now = deps.now ?? Date.now;
  const endpoint = muxLocalEndpoint();
  const current = await probe(endpoint);
  if (!current.reachable) return "already-absent";
  // An unproven identity is never a license to destroy PTYs: a process that
  // cannot prove it is this machine's keeper is refused even under force-live.
  requireAuthenticatedProof(current);
  const currentDigest = bindingDigest(current)!;
  const empty = current.bindings.length === 0
    && current.spawningChannels.length === 0
    && currentDigest === KEEPER_EMPTY_BINDING_DIGEST;
  if (!empty && !request.forceLive) {
    throw new Error("keeper refresh refused because the keeper has live channels");
  }
  if (!empty) {
    log.warn("keeper-update", "maintenance_forced_live_keeper_shutdown", {
      keeper_pid: current.keeperPid,
      keeper_epoch: current.processEpoch,
      binding_digest: currentDigest,
      channel_bindings: current.bindings.map(binding => ({
        channel_id: binding.channel_id,
        pid: binding.pid,
      })),
      spawning_channels: [...current.spawningChannels],
    });
  }
  const stopped = empty
    ? await shutdownEmpty(endpoint, {
        keeperPid: current.keeperPid,
        processEpoch: current.processEpoch,
        bindingDigest: currentDigest,
      })
    : await shutdownForced(endpoint);
  if (!stopped) throw new Error("authenticated keeper shutdown was rejected");
  await requireKeeperExit(probe, sleep, now);
  log.info("keeper-update", "maintenance_keeper_shutdown", {
    keeper_pid: current.keeperPid,
    keeper_epoch: current.processEpoch,
    forced_live: !empty,
  });
  return "shutdown";
}
