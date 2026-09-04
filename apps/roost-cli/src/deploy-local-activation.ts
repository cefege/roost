// Failure-safe activation for a staged localhost worker release. The local
// deploy driver supplies platform service commands; this module guarantees
// every install/restart/proof failure restores the prior worker first.

import { DeployFailure, workerServiceIsRunning, workerServiceMatchesRelease } from "./deploy-exec.ts";

export type LocalWorkerCommandResult = { exit: number; stdout: string; stderr: string };

export interface LocalWorkerActivation {
  install: () => Promise<LocalWorkerCommandResult>;
  restart: () => Promise<LocalWorkerCommandResult>;
  verify: () => Promise<LocalWorkerCommandResult>;
  rollback: () => Promise<string | null>;
  cleanupStage: () => Promise<void>;
}

async function failLocalActivation(
  deps: LocalWorkerActivation,
  exitCode: number,
  message: string,
): Promise<never> {
  let rollbackError: string | null;
  try {
    rollbackError = await deps.rollback();
  } catch (error) {
    rollbackError = `rollback failed: ${error instanceof Error ? error.message : String(error)}`;
  }
  if (!rollbackError) await deps.cleanupStage();
  throw new DeployFailure(
    exitCode,
    `${message}\n${rollbackError ?? "prior worker service restored"}`,
  );
}

export async function _activateLocalWorker(
  deps: LocalWorkerActivation,
  platform: "linux" | "darwin" = process.platform === "linux" ? "linux" : "darwin",
): Promise<{ install: LocalWorkerCommandResult; verify: LocalWorkerCommandResult }> {
  let install: LocalWorkerCommandResult;
  try {
    install = await deps.install();
  } catch (error) {
    return failLocalActivation(
      deps,
      5,
      `install.sh failed: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  if (install.exit !== 0) {
    return failLocalActivation(
      deps,
      5,
      `install.sh failed\n${install.stdout}\n${install.stderr}`,
    );
  }

  let restarted: LocalWorkerCommandResult;
  try {
    restarted = await deps.restart();
  } catch (error) {
    return failLocalActivation(
      deps,
      4,
      `restart failed: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  if (restarted.exit !== 0) {
    return failLocalActivation(
      deps,
      4,
      `restart failed (exit ${restarted.exit})\n${restarted.stdout}\n${restarted.stderr}`,
    );
  }

  let verify: LocalWorkerCommandResult;
  try {
    verify = await deps.verify();
  } catch (error) {
    return failLocalActivation(
      deps,
      8,
      `worker service verification failed: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  if (verify.exit !== 0 || !workerServiceIsRunning(verify.stdout, platform)
    || !workerServiceMatchesRelease(verify.stdout)) {
    return failLocalActivation(
      deps,
      verify.exit || 8,
      `worker service verification failed\n${verify.stdout}\n${verify.stderr}`,
    );
  }
  return { install, verify };
}
