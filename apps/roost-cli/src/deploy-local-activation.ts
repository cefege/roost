// Failure-safe localhost worker stop, service preparation, keeper cutover,
// restart, and proof. deploy-local.ts injects the journaled keeper callback;
// every failure restores the prior worker before its target stage is removed.

import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";
import {
  DeployFailure,
  run,
  workerServiceIsRunning,
  workerServiceMatchesRelease,
} from "./deploy-exec.ts";
import {
  launchdBootstrapWithRetryCmd,
  WORKER_AGENT,
  WORKER_UNIT,
} from "./service-ctl.ts";

export type LocalWorkerCommandResult = { exit: number; stdout: string; stderr: string };
export type LocalKeeperUpdateApplier = (
  workerFingerprint: string,
  update: Readonly<JournaledKeeperUpdateV1>,
  direction: "target" | "source",
) => Promise<void>;

export interface LocalWorkerActivation {
  install: () => Promise<LocalWorkerCommandResult>;
  stop: () => Promise<LocalWorkerCommandResult>;
  keeperUpdate: Readonly<JournaledKeeperUpdateV1> | null;
  workerFingerprint: string | null;
  applyKeeperUpdate: LocalKeeperUpdateApplier;
  restart: () => Promise<LocalWorkerCommandResult>;
  verify: () => Promise<LocalWorkerCommandResult>;
  rollback: () => Promise<string | null>;
  cleanupStage: () => Promise<void>;
}

const LOCAL_SYSTEMD_RUNTIME =
  `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}";`;

export async function stopLocalWorkerForActivation(
  os: "linux" | "darwin",
  cwd: string,
): Promise<LocalWorkerCommandResult> {
  const command = os === "linux"
    ? `${LOCAL_SYSTEMD_RUNTIME} stop_status=0; ` +
      `systemctl --user stop ${WORKER_UNIT} 2>/dev/null || stop_status=$?; ` +
      `state=$(systemctl --user is-active ${WORKER_UNIT} 2>/dev/null || true); ` +
      `case "$state" in inactive|failed|unknown) exit 0;; ` +
      `*) test "$stop_status" -ne 0 && exit "$stop_status"; exit 1;; esac`
    : `set -e; uid=$(id -u); job=gui/$uid/${WORKER_AGENT}; ` +
      `launchctl bootout "$job" 2>/dev/null || true; ` +
      `for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do ` +
      `if ! launchctl print "$job" >/dev/null 2>&1; then exit 0; fi; ` +
      `sleep 0.25; done; echo 'local worker stop did not settle' >&2; exit 1`;
  return run(["bash", "-lc", command], { cwd, quiet: true });
}

export function startLocalWorkerForActivation(
  os: "linux" | "darwin",
  servicePath: string,
  cwd: string,
  role: string,
): Promise<LocalWorkerCommandResult> {
  const command = os === "linux"
    ? `${LOCAL_SYSTEMD_RUNTIME} systemctl --user daemon-reload && ` +
      `systemctl --user enable ${WORKER_UNIT} && systemctl --user restart ${WORKER_UNIT}`
    : `${launchdBootstrapWithRetryCmd(WORKER_AGENT, servicePath, { role, reload: false })}; ` +
      `launchctl enable gui/$(id -u)/${WORKER_AGENT}; ` +
      `launchctl kickstart -k gui/$(id -u)/${WORKER_AGENT} 2>/dev/null || true`;
  return run(["bash", "-lc", command], { cwd, quiet: true });
}

export function reloadStoppedLocalWorkerDefinition(
  os: "linux" | "darwin",
  cwd: string,
): Promise<LocalWorkerCommandResult> {
  return os === "linux"
    ? run(
        ["bash", "-lc", `${LOCAL_SYSTEMD_RUNTIME} systemctl --user daemon-reload`],
        { cwd, quiet: true },
      )
    : Promise.resolve({ exit: 0, stdout: "", stderr: "" });
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
  if (deps.keeperUpdate) {
    if (!deps.workerFingerprint) {
      return failLocalActivation(deps, 5, "keeper update worker identity is missing");
    }
    try {
      await deps.applyKeeperUpdate(
        deps.workerFingerprint,
        deps.keeperUpdate,
        "target",
      );
    } catch (error) {
      return failLocalActivation(
        deps,
        5,
        `keeper update failed: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  }


  let stopped: LocalWorkerCommandResult;
  try {
    stopped = await deps.stop();
  } catch (error) {
    return failLocalActivation(
      deps,
      4,
      `worker stop failed: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  if (stopped.exit !== 0) {
    return failLocalActivation(
      deps,
      4,
      `worker stop failed (exit ${stopped.exit})\n${stopped.stdout}\n${stopped.stderr}`,
    );
  }
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
