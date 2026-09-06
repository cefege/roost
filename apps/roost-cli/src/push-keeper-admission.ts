// Atomic push keeper preflight classifies every registered POSIX worker from
// one coordinator snapshot. The caller must reject the complete batch before
// creating coordinator or worker journals when any participant is blocked.

import { join } from "node:path";
import {
  JournaledKeeperUpdateV1Schema,
  KeeperContractV1Schema,
  classifyKeeperUpdate,
  keeperUpdateAdmission,
  type KeeperContractV1,
  type KeeperUpdateClassification,
} from "@roost/shared/keeper-update";
import { sshExec } from "./deploy-exec.ts";
import type { FleetRolloutWorker } from "./push-fleet-rollout.ts";
import type { WorkerStatus } from "./status-types.ts";

export interface FleetKeeperAdmissionResult {
  workers: FleetRolloutWorker[];
  problems: string[];
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
  const problems: string[] = [];
  for (const target of targets) {
    const matchingWorkers = inventory.filter(
      worker => worker.fingerprint === target.fingerprint,
    );
    if (matchingWorkers.length !== 1) {
      problems.push(
        `${target.fingerprint}: update admission cannot resolve one worker`,
      );
      continue;
    }
    const worker = matchingWorkers[0]!;
    const targetContract = targetContracts.get(target.fingerprint);
    if (!targetContract) {
      problems.push(`${worker.label}: target keeper runtime proof is unavailable`);
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
      problems.push(classificationProblem(worker, classification));
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
  return { workers, problems };
}

function classificationProblem(
  worker: WorkerStatus,
  classification: KeeperUpdateClassification,
): string {
  switch (classification) {
    case "incompatible-with-live-sessions":
      return `${worker.label}: keeper update is incompatible with live sessions`;
    case "unproven":
      return `${worker.label}: keeper update admission is unproven`;
    case "worker-only-safe":
    case "keeper-restart-required":
      return `${worker.label}: keeper update admission metadata is incomplete`;
  }
}
