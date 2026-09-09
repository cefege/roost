#!/usr/bin/env bun
// The worker half of a release upgrade, driven by the product's own deploy
// code: source keeper contract, update admission, the coordinator-fenced keeper
// cutover, worker replacement, and the convergence proof. `roost deploy self`
// wraps these same calls around systemd/launchd, which a hermetic gate cannot
// own without restarting the developer's real worker, so this driver supplies
// the supervision. Exit 0 landed the upgrade; exit 5 is an admission refusal.

import { readFileSync, writeFileSync } from "node:fs";
import { setTimeout as delay } from "node:timers/promises";
import { z } from "zod";
import { buildAuthorizedApiClient } from "../../apps/roost-cli/src/api.ts";
import {
  createJournaledKeeperUpdateCallbacks,
  directKeeperUpdateAdmission,
} from "../../apps/roost-cli/src/direct-keeper-update.ts";
import {
  keeperAdmissionStaging,
  type KeeperAdmissionStaging,
} from "../../apps/roost-cli/src/keeper-admission-staging.ts";
import {
  loadSourceKeeperContract,
  targetKeeperContractForWorker,
} from "../../apps/roost-cli/src/push-keeper-admission.ts";
import { workerInventoryForUpdateAdmission } from "../../apps/roost-cli/src/status-report.ts";
import type { WorkerStatus } from "../../apps/roost-cli/src/status-types.ts";
import type { KeeperContractV1 } from "../../apps/shared/src/keeper-update.ts";
import { supportedHostPlatform } from "../../apps/shared/src/platform.ts";
import { createTerminalWorkerStarter } from "../terminal/stack-worker-runtime.ts";

const KEEPER_RPC_ATTEMPTS = 30;
const WORKER_EXIT_TIMEOUT_MS = 15_000;
/** Exit code the product uses for a deploy the target's own state refuses. */
const REFUSAL_EXIT_CODE = 5;

const WorkerServiceSpecSchema = z.object({
  label: z.string().min(1),
  home: z.string().min(1),
  logPath: z.string().min(1),
  dataDir: z.string().min(1),
  tmpDir: z.string().min(1),
  bootstrapToken: z.string().min(1),
  gitSha: z.string().min(1).optional(),
  shell: z.string().min(1).optional(),
}).strict();

export interface ReleaseHandoffRequest {
  workerServiceSpecPath: string;
  coordDbPath: string;
  coordinatorUrl: string;
  apiKeyPath: string;
  /** Deploy target, resolved against worker label, fingerprint or address. */
  host: string;
  sourceRoot: string;
  gitSha: string;
  installedWorkerPid: number;
  workerPidFilePath: string;
  forceLiveKeeperRetire: boolean;
}

/** A deploy the target's own keeper state refuses. Carries the product's
 *  message so the caller can assert on what an operator would read. */
export class KeeperAdmissionRefusal extends Error {}

export async function applyReleaseHandoff(request: ReleaseHandoffRequest): Promise<void> {
  const inventory = (): WorkerStatus[] => workerInventoryForUpdateAdmission(request.coordDbPath);
  const client = await buildAuthorizedApiClient({
    coordinatorUrl: request.coordinatorUrl,
    keyPath: request.apiKeyPath,
    label: "roost-upgrade-deploy",
  });

  const sourceContract = await loadSourceKeeperContract(request.sourceRoot);
  const targetContract = targetKeeperContractForWorker(sourceContract, request.gitSha, {
    bun_abi: Bun.version,
    platform: supportedHostPlatform(),
    arch: process.arch,
  });
  const staging = stageKeeperUpdate(request, targetContract, inventory());

  const callbacks = createJournaledKeeperUpdateCallbacks({
    prepare: (prepareRequest) => client.workersPrepareKeeperUpdate(prepareRequest),
    inventory,
    routable: async (workerFingerprint) =>
      (await client.workersList({})).routableFps.includes(workerFingerprint),
    sleep: delay,
    attempts: KEEPER_RPC_ATTEMPTS,
  });
  const { keeperUpdate, workerFingerprint } = staging;
  if (keeperUpdate !== null && workerFingerprint !== null) {
    console.log(`>> keeper ${keeperUpdate.admission.required_action} cutover on ${request.host}`);
    await callbacks.apply(workerFingerprint, keeperUpdate, "target");
  }

  await stopInstalledWorker(request.installedWorkerPid);
  const replacementPid = startReplacementWorker(request);
  console.log(`>> activated ${request.gitSha.slice(0, 8)} worker pid ${replacementPid}`);

  if (keeperUpdate !== null && workerFingerprint !== null) {
    await callbacks.prove(workerFingerprint, keeperUpdate, "target", request.gitSha, Date.now());
  } else {
    // No journaled update exists when the installed release could not prove a
    // keeper. The replacement still has to arrive and report one, or the next
    // upgrade inherits the same unprovable state.
    await proveKeeperRuntimeReported(inventory, request.host);
  }
  console.log(`>> done — ${request.host} worker upgraded to ${request.gitSha.slice(0, 8)}`);
}

export function parseReleaseHandoffRequest(args: readonly string[]): ReleaseHandoffRequest {
  const flag = (name: string): string => {
    const prefix = `--${name}=`;
    const match = args.find((argument) => argument.startsWith(prefix));
    if (match === undefined) throw new Error(`release handoff requires --${name}=<value>`);
    return match.slice(prefix.length);
  };
  const installedWorkerPid = Number(flag("installed-worker-pid"));
  if (!Number.isSafeInteger(installedWorkerPid) || installedWorkerPid <= 0) {
    throw new Error("--installed-worker-pid must be a running process id");
  }
  return {
    workerServiceSpecPath: flag("worker-service-spec"),
    coordDbPath: flag("coord-db"),
    coordinatorUrl: flag("coordinator-url"),
    apiKeyPath: flag("api-key"),
    host: flag("host"),
    sourceRoot: flag("source-root"),
    gitSha: flag("git-sha"),
    installedWorkerPid,
    workerPidFilePath: flag("worker-pid-file"),
    forceLiveKeeperRetire: args.includes("--force-live"),
  };
}

function stageKeeperUpdate(
  request: ReleaseHandoffRequest,
  targetContract: KeeperContractV1,
  snapshot: readonly WorkerStatus[],
): KeeperAdmissionStaging {
  try {
    // Throws when the registry proves a keeper the staged release can neither
    // adopt nor replace empty. That refusal is the contract under test: a live
    // PTY is never discarded on the operator's behalf.
    const resolved = directKeeperUpdateAdmission(
      request.host,
      targetContract,
      false,
      [...snapshot],
    );
    const staging = keeperAdmissionStaging(request.host, "local", resolved);
    if (staging.installedServiceRefusal !== null) {
      throw new Error(staging.installedServiceRefusal);
    }
    return staging;
  } catch (error) {
    throw new KeeperAdmissionRefusal(error instanceof Error ? error.message : String(error));
  }
}

function startReplacementWorker(request: ReleaseHandoffRequest): number {
  const spec = WorkerServiceSpecSchema.parse(
    JSON.parse(readFileSync(request.workerServiceSpecPath, "utf8")),
  );
  const replacement = createTerminalWorkerStarter(
    process.execPath,
    request.coordinatorUrl,
    request.sourceRoot,
  )({
    ...spec,
    gitSha: request.gitSha,
    forceLiveKeeperRetire: request.forceLiveKeeperRetire,
    detached: true,
  });
  const pid = replacement.child.pid;
  if (pid === undefined) throw new Error("replacement worker did not report a pid");
  writeFileSync(request.workerPidFilePath, `${pid}\n`);
  // The deploy hands the worker to the machine and exits, the way an installed
  // service outlives the tool that activated it.
  replacement.child.unref();
  return pid;
}

async function stopInstalledWorker(pid: number): Promise<void> {
  try {
    process.kill(pid, "SIGTERM");
  } catch {
    throw new Error(`installed worker pid ${pid} is not running; nothing to upgrade`);
  }
  const deadline = Date.now() + WORKER_EXIT_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      process.kill(pid, 0);
    } catch {
      return;
    }
    await delay(100);
  }
  throw new Error(`installed worker pid ${pid} did not exit within ${WORKER_EXIT_TIMEOUT_MS}ms`);
}

async function proveKeeperRuntimeReported(
  inventory: () => WorkerStatus[],
  host: string,
): Promise<void> {
  let problem = "worker did not report a keeper runtime observation";
  for (let attempt = 0; attempt < KEEPER_RPC_ATTEMPTS; attempt += 1) {
    try {
      const matches = inventory().filter((worker) => worker.label === host);
      const worker = matches.length === 1 ? matches[0] : undefined;
      if (worker && !worker.stale && worker.keeperRuntime) return;
      problem = worker
        ? `${host}: keeper runtime proof is still unavailable`
        : `${host}: deploy proof cannot resolve exactly one worker`;
    } catch (error) {
      problem = error instanceof Error ? error.message : String(error);
    }
    await delay(1_000);
  }
  throw new Error(`keeper runtime admission proof failed: ${problem}`);
}

if (import.meta.main) {
  try {
    await applyReleaseHandoff(parseReleaseHandoffRequest(process.argv.slice(2)));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    if (error instanceof KeeperAdmissionRefusal) {
      console.error(
        "  --force-live is the only authorization to retire a keeper holding live PTYs;"
        + " without it this deploy refuses rather than discarding them.",
      );
      process.exit(REFUSAL_EXIT_CODE);
    }
    process.exit(1);
  }
}
