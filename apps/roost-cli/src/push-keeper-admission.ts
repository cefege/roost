// Push keeper preflight classifies every rollout participant from one
// coordinator snapshot. A participant whose keeper cannot be updated safely is
// DEFERRED, not fatal: refusing the batch would let one unadoptable keeper
// block every other machine, and forcing it would end live PTYs.

import { join } from "node:path";
import {
  JournaledKeeperUpdateV1Schema,
  KeeperContractV1Schema,
  classifyKeeperUpdate,
  keeperUpdateAdmission,
  type KeeperContractV1,
  type KeeperUpdateClassification,
} from "@roost/protocol/keeper-update";
import { sshExec } from "./deploy-exec.ts";
import { _isSelfHost } from "./deploy-self-host.ts";
import type { FleetRolloutWorker } from "./push-fleet-rollout.ts";
import type { WorkerStatus } from "./status-types.ts";
import type { DeferredFleetWorker } from "./push-fleet-plan.ts";

export interface FleetKeeperAdmissionResult {
  workers: FleetRolloutWorker[];
  /** Participants this push must skip, each with its operator-facing reason. */
  deferred: DeferredFleetWorker[];
}
export function sourceKeeperContractCommand(
  sourceRoot: string,
  findBun: () => string | null = () => Bun.which("bun"),
  processExecutable = process.execPath,
): string[] {
  return [
    findBun() ?? processExecutable,
    join(sourceRoot, "apps", "roost-cli", "src", "main.ts"),
    "__keeper-contract",
  ];
}

export async function loadSourceKeeperContract(
  sourceRoot: string,
): Promise<KeeperContractV1> {
  const child = Bun.spawn(sourceKeeperContractCommand(sourceRoot), {
    cwd: sourceRoot,
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  });
  const [exitCode, stdout, stderr] = await Promise.all([
    child.exited,
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
  ]);
  if (exitCode !== 0) {
    throw new Error(
      `source keeper contract probe failed: ${stderr.trim() || `exit ${exitCode}`}`,
    );
  }
  try {
    return KeeperContractV1Schema.parse(JSON.parse(stdout));
  } catch (error) {
    throw new Error(`source keeper contract is malformed: ${String(error)}`);
  }
}


export async function probeTargetKeeperContract(
  host: string,
  targetSha: string,
  sourceContract: KeeperContractV1,
  runRemote: typeof sshExec = sshExec,
): Promise<KeeperContractV1> {
  if (await _isSelfHost(host)) {
    const platform = process.platform;
    if (platform !== "darwin" && platform !== "linux") {
      throw new Error(`${host}: local keeper runtime is unsupported`);
    }
    return targetKeeperContractForWorker(sourceContract, targetSha, {
      bun_abi: Bun.version,
      platform,
      arch: process.arch,
    });
  }
  const result = await runRemote(
    host,
    `bun -e 'process.stdout.write(JSON.stringify({bun_abi:Bun.version,platform:process.platform,arch:process.arch}))'`,
  );
  if (result.exit !== 0) {
    throw new Error(
      `${host}: target keeper runtime probe failed: ${result.stderr.trim() || `exit ${result.exit}`}`,
    );
  }
  let environment: unknown;
  try {
    environment = JSON.parse(result.stdout);
  } catch {
    throw new Error(`${host}: target keeper runtime probe returned malformed JSON`);
  }
  if (!environment || typeof environment !== "object"
    || Array.isArray(environment)) {
    throw new Error(`${host}: target keeper runtime probe returned malformed metadata`);
  }
  const fields = environment as Record<string, unknown>;
  if (typeof fields.bun_abi !== "string"
    || (fields.platform !== "darwin" && fields.platform !== "linux")
    || typeof fields.arch !== "string") {
    throw new Error(`${host}: target keeper runtime metadata is unsupported`);
  }
  return targetKeeperContractForWorker(sourceContract, targetSha, {
    bun_abi: fields.bun_abi,
    platform: fields.platform,
    arch: fields.arch,
  });

}

export function targetKeeperContractForWorker(
  sourceContract: KeeperContractV1,
  targetSha: string,
  targetEnvironment: Pick<KeeperContractV1, "bun_abi" | "platform" | "arch">,
): KeeperContractV1 {
  return {
    ...sourceContract,
    ...targetEnvironment,
    build_sha: targetSha,
  };
}

export function classifyFleetKeeperUpdates(
  targets: readonly Pick<FleetRolloutWorker, "fingerprint" | "host">[],
  inventory: readonly WorkerStatus[],
  targetContracts: ReadonlyMap<string, KeeperContractV1>,
): FleetKeeperAdmissionResult {
  const workers: FleetRolloutWorker[] = [];
  const deferred: DeferredFleetWorker[] = [];
  for (const target of targets) {
    const matchingWorkers = inventory.filter(
      worker => worker.fingerprint === target.fingerprint,
    );
    if (matchingWorkers.length !== 1) {
      deferred.push({
        fingerprint: target.fingerprint,
        label: target.fingerprint.slice(0, 12),
        reason: "update admission cannot resolve one worker",
      });
      continue;
    }
    const worker = matchingWorkers[0]!;
    const targetContract = targetContracts.get(target.fingerprint);
    if (!targetContract) {
      deferred.push({
        fingerprint: target.fingerprint,
        label: worker.label,
        reason: "target keeper runtime proof is unavailable",
      });
      continue;
    }
    const openSessions = new Set(worker.coordinatorOpenSessionIds);
    const classification = classifyKeeperUpdate(
      targetContract,
      worker.keeperRuntime,
      openSessions,
    );
    const admission = keeperUpdateAdmission(
      targetContract,
      worker.keeperRuntime,
      openSessions,
    );
    if (!admission || !worker.keeperRuntime) {
      deferred.push({
        fingerprint: target.fingerprint,
        label: worker.label,
        reason: keeperDeferralReason(classification),
      });
      continue;
    }
    workers.push({
      fingerprint: target.fingerprint,
      host: target.host,
      keeperUpdate: JournaledKeeperUpdateV1Schema.parse({
        admission,
        source_contract: worker.keeperRuntime.running_contract,
        target_contract: targetContract,
      }),
    });
  }
  return { workers, deferred };
}

/** Each reason names the operator's own way out, because a keeper deferral is
 *  the one kind that does not clear itself on the machine's next attach. */
function keeperDeferralReason(classification: KeeperUpdateClassification): string {
  switch (classification) {
    case "incompatible-with-live-sessions":
      return "keeper cannot be adopted while its sessions are live —"
        + " `roost keeper-refresh <host> --yes` when those PTYs are expendable";
    case "unproven":
      return "keeper update admission is unproven";
    case "worker-only-safe":
    case "keeper-restart-required":
      return "keeper update admission metadata is incomplete";
  }
}

