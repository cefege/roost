// The two signed-Windows-update drivers over the coordinator's SCM broker,
// deliberately distinct despite the similar names:
//   - deployWindowsWorkerViaCoordinator / tryCoordinatorWindowsDeploy update
//     a remote WORKER through coord RPC (workersDeployStart/Output) and are
//     called from `roost deploy <host>`.
//   - tryCoordinatorSelfUpdate updates THE COORDINATOR ITSELF on Windows
//     through windows-update-control's local broker, returning null on POSIX
//     so `roost push` falls back to source deployment.
// Also hosts normalizedHost, shared by both fleets' target matching.
// Callers: deploy.ts (worker twin), push.ts + main/update flows (self-update).

import { randomUUID } from "node:crypto";
import type { CoordClient } from "../../worker/src/coord-client.ts";
import { loadWorkerConfig } from "../../worker/src/config.ts";
import { buildApiClient, buildSelfAuthorizedApiClient } from "./api.ts";
import { DeployFailure, failDeploy } from "./deploy-exec.ts";
import { loadWindowsServiceDefinitions } from "./service-ctl.ts";
import { statusReport } from "./status.ts";
import { fetchAndVerifyReleaseAsset, WINDOWS_RELEASE_MANIFEST_ASSET } from "./update.ts";
import { parseWindowsReleaseManifest } from "./windows/windows-update-journal.ts";
import { coordinatorReportIsHealthy } from "./coordinator-deploy-recovery.ts";

type WindowsDeployClient = Pick<CoordClient, "workersList" | "workersDeployStart" | "workersDeployOutput">;
type WindowsDeployRetryOptions = {
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
  deadlineMs?: number;
  expectedGitSha?: string;
  expectedManifestSha256?: string;
};

const WINDOWS_DEPLOY_RECONNECT_DEADLINE_MS = 16 * 60 * 1000;

export function normalizedHost(value: string): string {
  return value.trim().replace(/\.$/, "").toLowerCase();
}

/** Drive the coordinator-owned signed update channel when `host` is Windows. */
export async function deployWindowsWorkerViaCoordinator(
  client: WindowsDeployClient,
  host: string,
  log: (line: string) => void = console.log,
  retryOptions: WindowsDeployRetryOptions = {},
): Promise<boolean> {
  const now = retryOptions.now ?? Date.now;
  const sleep = retryOptions.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const deadline = now() + (retryOptions.deadlineMs ?? WINDOWS_DEPLOY_RECONNECT_DEADLINE_MS);
  const requested = normalizedHost(host);
  if (!requested) failDeploy(2, "deploy target must not be empty");
  const inventory = await client.workersList({}).catch(() => null);
  // Client creation is lazy. An unreachable coordinator must not block the
  // existing direct SSH path before we have positively identified Windows.
  if (!inventory) return false;

  const fingerprintMatches = inventory.workers.filter(
    (candidate) => normalizedHost(candidate.fp) === requested,
  );
  if (fingerprintMatches.length > 1) {
    failDeploy(2, `ambiguous deploy target "${host}" matches multiple worker fingerprints`);
  }
  const aliasMatches = fingerprintMatches.length === 0
    ? inventory.workers.filter((candidate) =>
        [candidate.label, candidate.reachableAddr ?? ""].some((identity) => {
          const normalized = normalizedHost(identity);
          return normalized.length > 0 && normalized === requested;
        }))
    : [];
  if (aliasMatches.length > 1) {
    failDeploy(
      2,
      `ambiguous deploy target "${host}" matches multiple registered workers; use the worker fingerprint`,
  );
  }
  const worker = fingerprintMatches[0] ?? aliasMatches[0];
  if (worker?.os !== "win32") return false;

  const started = await client.workersDeployStart({
    host: worker.fp,
    expectedGitSha: retryOptions.expectedGitSha,
    expectedManifestSha256: retryOptions.expectedManifestSha256,
  });
  if (!started.ok || !started.jobId) {
    failDeploy(2, started.error || `failed to start signed Windows update for ${worker.label}`);
  }
  log(`>> signed Windows update ${worker.label} (${started.jobId})`);

  let completed = false;
  let reconnectAttempt = 0;
  const deliveredLines: string[] = [];
  while (!completed) {
    let lineIndex = 0;
    try {
      for await (const frame of client.workersDeployOutput({ jobId: started.jobId })) {
        if (frame.kind === "line") {
          if (frame.text) {
            if (deliveredLines[lineIndex] !== frame.text) {
              deliveredLines.splice(lineIndex);
              deliveredLines.push(frame.text);
              log(frame.text);
            }
            lineIndex++;
          }
          continue;
        }
        if (frame.kind !== "done") continue;
        if (frame.exit !== 0) {
          if (frame.error === "unknown jobId" && now() < deadline) break;
          failDeploy(2, frame.error || `signed Windows update failed with exit ${frame.exit}`);
        }
        completed = true;
        log(`✓ signed Windows update complete for ${worker.label}`);
        break;
      }
    } catch (error) {
      if (error instanceof DeployFailure) throw error;
      if (now() >= deadline) {
        failDeploy(2, `signed Windows update stream did not recover: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
    if (completed) break;
    if (now() >= deadline) {
      failDeploy(2, "signed Windows update stream ended without a terminal result");
    }
    reconnectAttempt++;
    const delayMs = Math.min(250 * (2 ** Math.min(reconnectAttempt - 1, 3)), 2_000);
    log(`>> coordinator stream unavailable; reconnecting to Windows update ${started.jobId}`);
    await sleep(delayMs);
  }
  return true;
}

export async function tryCoordinatorWindowsDeploy(
  host: string,
  expectedGitSha?: string,
  expectedManifestSha256?: string,
): Promise<boolean> {
  let client: CoordClient;
  try {
    if (process.platform === "win32") {
      const definitions = await loadWindowsServiceDefinitions();
      const config = loadWorkerConfig(definitions.worker.environment);
      client = await buildApiClient({ coordinatorUrl: config.coordinatorUrl });
    } else {
      client = await buildSelfAuthorizedApiClient();
    }
  } catch {
    return false;
  }
  return deployWindowsWorkerViaCoordinator(client, host, console.log, {
    expectedGitSha,
    expectedManifestSha256,
  });
}

interface LocalWindowsUpdateFrame {
  sequence: number;
  phase: string;
  message: string;
  terminal: boolean;
  success: boolean;
  error: string;
}

export interface WindowsCoordinatorDeployOptions {
  platform?: NodeJS.Platform;
  timeoutMs?: number;
  now?: () => number;
  sleep?: (milliseconds: number) => Promise<void>;
  start?: (expectedSha: string) => Promise<{ jobId: string; frames: LocalWindowsUpdateFrame[] }>;
  status?: (jobId: string, afterSequence: number) => Promise<LocalWindowsUpdateFrame[]>;
  log?: (message: string) => void;
  prove?: (expectedSha: string) => Promise<boolean>;
  current?: (expectedSha: string) => Promise<boolean>;
}

/** Drive the COORDINATOR's own self-update through the signed Windows SCM
 * broker (deploy.ts keeps the WORKER-update twin under the windows name).
 * POSIX returns null so the caller uses source deployment. */
export async function tryCoordinatorSelfUpdate(
  expectedSha: string,
  options: WindowsCoordinatorDeployOptions = {},
): Promise<boolean | null> {
  if ((options.platform ?? process.platform) !== "win32") return null;
  const now = options.now ?? Date.now;
  const sleep = options.sleep ?? Bun.sleep;
  const log = options.log ?? ((message: string) => console.log(message));
  const timeoutMs = options.timeoutMs ?? 5 * 60_000;
  const prove = options.prove ?? (async (sha: string) =>
    coordinatorReportIsHealthy(await statusReport(), sha));
  const current = options.current ?? prove;
  try {
    if (await current(expectedSha)) {
      log("Windows coordinator already reports the exact target build");
      return true;
    }
  } catch {
    // A stale or temporarily unavailable coordinator still needs the updater
    // transaction below. The post-update proof remains authoritative.
  }
  let defaultStatus: WindowsCoordinatorDeployOptions["status"];
  const started = options.start
    ? await options.start(expectedSha)
    : await (async () => {
      const release = await fetchAndVerifyReleaseAsset(WINDOWS_RELEASE_MANIFEST_ASSET, {
        subject: "Windows coordinator manifest",
        timeoutMs: 30_000,
        checksumTimeoutMs: 30_000,
        fail: (message) => new DeployFailure(8, message),
      });
      const manifest = parseWindowsReleaseManifest(release.bytes);
      if (manifest.build !== expectedSha) {
        throw new DeployFailure(
          8,
          `signed Windows release reports ${manifest.build}, expected source commit ${expectedSha}`,
        );
      }
      // Dynamic import boundary: windows-update-control binds the Windows-only
      // SCM/native broker surface and must never be loaded on a POSIX host
      // taking the source-deployment path.
      const { handleUpdateBrokerCommand } = await import("./windows/windows-update-control.ts");
      const jobId = randomUUID();
      const command = {
        requestId: randomUUID(),
        jobId,
        action: "START" as const,
        manifestUrl: release.url,
        signatureUrl: `${release.url}.p7s`,
        manifestSha256: release.sha256,
        publisherSha256: "",
      };
      const frames = await handleUpdateBrokerCommand(command);
      defaultStatus = async (statusJobId, afterSequence) =>
        await handleUpdateBrokerCommand({
          ...command,
          requestId: randomUUID(),
          jobId: statusJobId,
          action: "STATUS",
          afterSequence,
        });
      return { jobId, frames };
    })();
  const readStatus = options.status ?? defaultStatus;
  if (!readStatus) throw new Error("Windows coordinator update status reader is unavailable");

  let afterSequence = 0;
  let frames = started.frames;
  const deadline = now() + timeoutMs;
  for (;;) {
    for (const frame of frames) {
      if (frame.sequence <= afterSequence) continue;
      afterSequence = frame.sequence;
      log(`>> [${frame.phase}] ${frame.message}`);
      if (frame.terminal) {
        if (!frame.success) {
          throw new DeployFailure(8, frame.error || `Windows coordinator update failed in ${frame.phase}`);
        }
        if (!(await prove(expectedSha))) {
          throw new DeployFailure(
            8,
            `Windows coordinator did not report healthy build ${expectedSha} after durable update success`,
          );
        }
        return true;
      }
    }
    if (now() >= deadline) {
      throw new DeployFailure(
        8,
        `Windows coordinator update ${started.jobId} did not reach durable terminal success within ${timeoutMs}ms`,
      );
    }
    await sleep(250);
    frames = await readStatus(started.jobId, afterSequence);
  }
}
