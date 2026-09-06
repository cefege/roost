// Keeper survivor admission for worker boot. Authenticated protocol-compatible
// survivors are adopted; automatic replacement is identity-fenced and allowed
// only after coordinator sessions and keeper channel bindings prove empty.

import {
  cleanupLocalEndpoint,
  type LocalEndpoint,
} from "@roost/shared/local-endpoint";
import { log } from "@roost/shared/log";
import { KEEPER_EMPTY_BINDING_DIGEST } from "@roost/shared/keeper-update";
import {
  getMultiplexedPool,
  probeKeeperCompatible,
  shutdownEmptyKeeperAuthenticated,
} from "./keeper/multiplexed-client.ts";
import { muxLocalEndpoint } from "./keeper/keeper-pool-config.ts";
import { KEEPER_TARGET_CONTRACT } from "./keeper/keeper-stamp.ts";

export const KEEPER_REPLACEMENT_BLOCKED_ERROR =
  "keeper replacement blocked by live sessions";

async function waitForKeeperExit(endpoint: LocalEndpoint): Promise<boolean> {
  const deadline = Date.now() + 2_000;
  do {
    const probe = await probeKeeperCompatible(endpoint, 250);
    if (!probe.reachable) return true;
    await Bun.sleep(25);
  } while (Date.now() < deadline);
  return false;
}

export async function handleKeeperSurvivor(
  coordinatorOpenSessionIds: ReadonlySet<string>,
): Promise<void> {
  const endpoint = muxLocalEndpoint();
  const probe = await probeKeeperCompatible(endpoint);
  if (!probe.reachable) {
    await cleanupLocalEndpoint(endpoint);
    return;
  }

  if (probe.authenticated && probe.protocolCompatible) {
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

  const keeperProvesEmpty =
    probe.authenticated
    && probe.keeperPid !== undefined
    && probe.processEpoch !== undefined
    && probe.bindings?.length === 0
    && probe.spawningChannels?.length === 0;
  if (coordinatorOpenSessionIds.size > 0 || !keeperProvesEmpty) {
    log.warn("worker", "keeper_survivor_replacement_blocked", {
      endpoint: endpoint.address,
      kind: endpoint.kind,
      authenticated: probe.authenticated,
      protocol_compatible: probe.protocolCompatible,
      exact_target: probe.exactTarget,
      coordinator_sessions: coordinatorOpenSessionIds.size,
      keeper_bindings: probe.bindings?.length ?? null,
      spawning_channels: probe.spawningChannels?.length ?? null,
      keeper_pid: probe.keeperPid ?? null,
      process_epoch: probe.processEpoch ?? null,
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
    keeperPid: probe.keeperPid!,
    processEpoch: probe.processEpoch!,
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
