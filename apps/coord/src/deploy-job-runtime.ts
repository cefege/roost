// Supervised POSIX worker-update subprocess runtime. The durable job owner
// controls admission, persistence, and retries; this module owns one child,
// bounded output, structured failures, and platform-owner settlement proof.

import { IS_COMPILED_ROOST_BUILD } from "@roost/shared/build-identity";
import type {
  WorkerUpdateFailure,
  WorkerUpdateProgressEvent,
  WorkerUpdateStartRequest,
} from "@roost/shared/worker-update-operation";
import { consumeWorkerUpdateChildFailure } from "@roost/shared/worker-update-result-channel";
import { normalizeDeployOutputLine } from "./deploy-job-record.ts";

export const POSIX_DEPLOY_TIMEOUT_MS = 20 * 60 * 1_000;

export interface DeployJobRuntimeCallbacks {
  onLine(line: string): Promise<void> | void;
  onProgress(event: WorkerUpdateProgressEvent): Promise<void> | void;
}

export interface DeployJobRuntimeResult {
  exitCode: number | null;
  timedOut: boolean;
  settlementProven: boolean;
  error: string | null;
  failure: WorkerUpdateFailure | null;
}

export interface DeployJobRuntimeHandle {
  result: Promise<DeployJobRuntimeResult>;
  stop(): void;
}


export function startDeployJobRuntime(
  request: WorkerUpdateStartRequest,
  jobId: string,
  coordinatorUrl: string,
  callbacks: DeployJobRuntimeCallbacks,
  timeoutMs = POSIX_DEPLOY_TIMEOUT_MS,
): DeployJobRuntimeHandle {
  const bunExecutable = process.execPath;
  const deployArgs = [
    "deploy",
    request.host,
    `--source-root=${request.sourceRoot}`,
    `--expected-sha=${request.expectedGitSha}`,
    "--pinned-source",
    `--deploy-job-id=${jobId}`,
  ];
  const command = IS_COMPILED_ROOST_BUILD
    ? [bunExecutable, ...deployArgs]
    : [bunExecutable, "apps/roost-cli/src/main.ts", ...deployArgs];
  const resultId = crypto.randomUUID();
  const environment = {
    ...process.env,
    ROOST_COORDINATOR_URL: coordinatorUrl,
    ROOST_DEPLOY_WORKER_FP: request.workerFp,
    ROOST_DEPLOY_RESULT_ID: resultId,
  };
  let child: ReturnType<typeof Bun.spawn>;
  try {
    child = Bun.spawn({
      cmd: command,
      cwd: request.sourceRoot,
      env: environment,
      stdout: "pipe",
      stderr: "pipe",
    });
  } catch (error) {
    return {
      stop: () => {},
      result: Promise.resolve({
        exitCode: null,
        timedOut: false,
        settlementProven: false,
        error: error instanceof Error ? error.message : String(error),
        failure: null,
      }),
    };
  }

  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    try {
      child.kill();
    } catch {
      // The supervised child already settled.
    }
  }, timeoutMs);

  const consumeLine = async (raw: string): Promise<void> => {
    const normalized = normalizeDeployOutputLine(raw);
    if (normalized) await callbacks.onLine(normalized);
  };

  const pump = async (stream: ReadableStream<Uint8Array>): Promise<void> => {
    const reader = stream.getReader();
    const decoder = new TextDecoder();
    let buffered = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buffered += decoder.decode(value, { stream: true });
      let newline: number;
      while ((newline = buffered.indexOf("\n")) !== -1) {
        await consumeLine(buffered.slice(0, newline));
        buffered = buffered.slice(newline + 1);
      }
    }
    buffered += decoder.decode();
    if (buffered) await consumeLine(buffered);
  };

  const result = Promise.all([
    pump(child.stdout as ReadableStream<Uint8Array>),
    pump(child.stderr as ReadableStream<Uint8Array>),
    child.exited,
  ]).then(async ([, , exitCode]) => {
    const settlementProven = !timedOut && exitCode === 0
      ? await provePlatformSettlement(command, request, environment)
      : false;
    if (settlementProven) {
      await callbacks.onProgress({
        atMs: Date.now(),
        phase: "settled",
        message: "Host journal settled",
      });
    }
    const failure = await consumeWorkerUpdateChildFailure(
      request.workerFp,
      jobId,
      resultId,
    );
    return {
      exitCode: exitCode ?? null,
      timedOut,
      settlementProven,
      error: timedOut
        ? `deploy timed out after ${Math.floor(timeoutMs / 1_000)}s`
        : null,
      failure,
    };
  }).catch(async (error) => ({
    exitCode: null,
    timedOut,
    settlementProven: false,
    error: error instanceof Error ? error.message : String(error),
    failure: await consumeWorkerUpdateChildFailure(request.workerFp, jobId, resultId),
  })).finally(() => clearTimeout(timer));

  return {
    result,
    stop: () => {
      try {
        child.kill();
      } catch {
        // The supervised child already settled.
      }
    },
  };
}


async function provePlatformSettlement(
  primaryCommand: string[],
  request: WorkerUpdateStartRequest,
  environment: Record<string, string | undefined>,
): Promise<boolean> {
  const command = IS_COMPILED_ROOST_BUILD
    ? [primaryCommand[0]!, "__deploy-settlement-probe", request.host]
    : [
        primaryCommand[0]!,
        "apps/roost-cli/src/main.ts",
        "__deploy-settlement-probe",
        request.host,
      ];
  try {
    const probe = Bun.spawn({
      cmd: command,
      cwd: request.sourceRoot,
      env: environment,
      stdout: "ignore",
      stderr: "ignore",
    });
    return await probe.exited === 0;
  } catch {
    return false;
  }
}
