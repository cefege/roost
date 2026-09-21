// Harness-only coordinator entrypoint for independent-update upgrade proof.
// It injects applyReleaseHandoff at the durable job executor boundary while
// ordinary coordinator binaries retain the production POSIX runtime.

import { readFileSync } from "node:fs";
import { Database } from "bun:sqlite";
import { z } from "zod";
import { runCoord } from "../../apps/coord/src/main.ts";
import { __setConnectWorkerForTest } from "../../apps/coord/src/connect/worker-registry.ts";
import type {
  DeployJobRuntimeCallbacks,
  DeployJobRuntimeHandle,
  DeployJobRuntimeResult,
} from "../../apps/coord/src/deploy-job-runtime.ts";
import type { WorkerUpdateStartRequest } from "../../apps/shared/src/worker-update-operation.ts";
import { applyReleaseHandoff } from "./release-handoff.ts";

const HandoffSchema = z.object({
  workerServiceSpecPath: z.string().min(1),
  coordDbPath: z.string().min(1),
  coordinatorUrl: z.string().url(),
  apiKeyPath: z.string().min(1),
  host: z.string().min(1),
  sourceRoot: z.string().min(1),
  gitSha: z.string().regex(/^[0-9a-f]{40}$/),
  installedWorkerPid: z.number().int().positive(),
  workerPidFilePath: z.string().min(1),
  forceLiveKeeperRetire: z.boolean(),
}).strict();
const RuntimeConfigSchema = z.object({
  failedWorkerFp: z.string().regex(/^[0-9a-f]{64}$/),
  offlineWorkerFp: z.string().regex(/^[0-9a-f]{64}$/),
  offlineWorkerOnline: z.boolean(),
  handoff: HandoffSchema,
}).strict();
type RuntimeConfig = z.infer<typeof RuntimeConfigSchema>;

const configArgument = process.argv.find(argument =>
  argument.startsWith("--update-runtime-config="));
if (!configArgument) throw new Error("independent update coordinator requires runtime config path");
const runtimeConfigPath = configArgument.slice("--update-runtime-config=".length);
const initial = readConfig();
installFakeWorkerHandle(initial.failedWorkerFp);
const configWatcher = setInterval(() => {
  const config = readConfig();
  if (config.offlineWorkerOnline) installFakeWorkerHandle(config.offlineWorkerFp);
}, 100);
configWatcher.unref();

function startRuntime(
  request: WorkerUpdateStartRequest,
  _jobId: string,
  _coordinatorUrl: string,
  callbacks: DeployJobRuntimeCallbacks,
): DeployJobRuntimeHandle {
  let stopped = false;
  const result = (async (): Promise<DeployJobRuntimeResult> => {
    const config = readConfig();
    if (request.workerFp === config.failedWorkerFp) {
      return failedResult("Injected worker activation failure");
    }
    if (stopped) return failedResult("Executor stopped");
    if (request.workerFp === config.offlineWorkerFp) {
      const db = new Database(config.handoff.coordDbPath);
      try {
        db.exec("PRAGMA busy_timeout=10000");
        db.query(
          "UPDATE workers SET git_sha = ?, last_seen_ms = ? WHERE fp = ?",
        ).run(request.expectedGitSha, Date.now(), request.workerFp);
      } finally {
        db.close();
      }
    } else {
      await applyReleaseHandoff(config.handoff);
    }
    await callbacks.onProgress({
      atMs: Date.now(),
      phase: "settled",
      message: "Host journal settled",
    });
    return {
      exitCode: 0,
      timedOut: false,
      settlementProven: true,
      error: null,
      failure: null,
    };
  })();
  return { result, stop: () => { stopped = true; } };
}

function failedResult(message: string): DeployJobRuntimeResult {
  return {
    exitCode: 8,
    timedOut: false,
    settlementProven: false,
    error: message,
    failure: {
      code: "deploy_failed",
      phase: "activation",
      message,
      journal: null,
      expectedKeeper: null,
      observedKeeper: null,
      targetContract: null,
    },
  };
}

function installFakeWorkerHandle(workerFp: string): void {
  __setConnectWorkerForTest(workerFp, { workerFp, send: () => 1 });
}

function readConfig(): RuntimeConfig {
  return RuntimeConfigSchema.parse(JSON.parse(readFileSync(runtimeConfigPath, "utf8")));
}

await runCoord({
  startWorkerUpdateRuntime: startRuntime,
  enableWorkerCatchUp: false,
});
