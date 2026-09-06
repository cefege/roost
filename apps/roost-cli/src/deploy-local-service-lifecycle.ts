// Exact local worker lifecycle capture and rollback restoration.
// The deploy journal records this state before mutation; recovery temporarily
// starts the prior worker for authenticated keeper RPC, then restores it.

import type {
  LocalWorkerDeployJournal,
  LocalWorkerLifecycle,
  LocalWorkerStartupPolicy,
} from "./local-worker-deploy-journal.ts";
import { stopLocalWorkerForActivation } from "./deploy-local-activation.ts";
import {
  run,
  workerServiceIsRunning,
} from "./deploy-exec.ts";
import {
  launchdBootstrapWithRetryCmd,
  verifyWorkerCmd,
  WORKER_AGENT,
  WORKER_UNIT,
} from "./service-ctl.ts";

const SYSTEMD_RUNTIME =
  `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}";`;

export async function probeLocalWorkerLifecycle(
  os: "linux" | "darwin",
): Promise<LocalWorkerLifecycle> {
  const status = await run(["bash", "-lc", verifyWorkerCmd(os)], { quiet: true });
  if (status.exit === 0 && workerServiceIsRunning(status.stdout, os)) return "running";
  if (os === "darwin") {
    if (status.exit === 0) return "stopped";
    if (status.exit === 1) return "unloaded";
    return "unknown";
  }
  if (status.exit === 0) return "stopped";
  const active = await run([
    "bash",
    "-lc",
    `${SYSTEMD_RUNTIME} systemctl --user is-active ${WORKER_UNIT} 2>/dev/null`,
  ], { quiet: true });
  return ["inactive", "failed", "unknown", "deactivating"].includes(active.stdout.trim())
    ? "stopped"
    : "unknown";
}

export async function probeLocalWorkerStartupPolicy(
  os: "linux" | "darwin",
): Promise<Exclude<LocalWorkerStartupPolicy, "absent">> {
  if (os === "linux") {
    const result = await run([
      "bash",
      "-lc",
      `${SYSTEMD_RUNTIME} systemctl --user is-enabled ${WORKER_UNIT} 2>/dev/null`,
    ], { quiet: true });
    const policy = result.stdout.trim();
    if (policy === "enabled" || policy === "disabled" || policy === "masked") {
      return policy;
    }
    throw new Error(`cannot capture worker startup policy: ${policy || result.stderr.trim()}`);
  }
  const result = await run([
    "bash",
    "-lc",
    `launchctl print-disabled gui/$(id -u)`,
  ], { quiet: true });
  if (result.exit !== 0) {
    throw new Error(`cannot capture worker disabled override: ${result.stderr.trim()}`);
  }
  const label = WORKER_AGENT.replaceAll(".", "[.]");
  const match = result.stdout.match(
    new RegExp(`"${label}"\\s*=>\\s*(true|false|disabled|enabled)`),
  );
  return match && (match[1] === "true" || match[1] === "disabled")
    ? "disabled"
    : "enabled";
}

export async function readLocalWorkerPriorState(
  os: "linux" | "darwin",
  serviceInstalled: boolean,
): Promise<{
  lifecycle: Exclude<LocalWorkerLifecycle, "unknown">;
  startupPolicy: LocalWorkerStartupPolicy;
}> {
  const lifecycle = await probeLocalWorkerLifecycle(os);
  if (lifecycle === "unknown") throw new Error("cannot capture worker lifecycle");
  if (!serviceInstalled) {
    if (lifecycle === "running" || (os === "darwin" && lifecycle === "stopped")) {
      throw new Error("worker is loaded without a restorable service definition");
    }
    return { lifecycle: "unloaded", startupPolicy: "absent" };
  }
  if (os === "linux" && lifecycle === "unloaded") {
    throw new Error("installed Linux worker lifecycle is unavailable");
  }
  return { lifecycle, startupPolicy: await probeLocalWorkerStartupPolicy(os) };
}

async function checkedLifecycleCommand(
  result: { exit: number; stdout: string; stderr: string },
  operation: string,
): Promise<void> {
  if (result.exit === 0) return;
  throw new Error(
    `${operation} failed (exit ${result.exit})\n${result.stdout}\n${result.stderr}`,
  );
}

export async function startRestoredLocalWorker(
  journal: Readonly<LocalWorkerDeployJournal>,
  servicePath: string,
): Promise<void> {
  if (!journal.priorService) return;
  let result;
  if (journal.os === "linux") {
    result = await run([
      "bash",
      "-lc",
      `${SYSTEMD_RUNTIME} systemctl --user unmask ${WORKER_UNIT} 2>/dev/null || true; ` +
        `systemctl --user daemon-reload && ` +
        `(systemctl --user reset-failed ${WORKER_UNIT} 2>/dev/null || true) && ` +
        `systemctl --user start ${WORKER_UNIT}`,
    ], { cwd: journal.sourceRoot, quiet: true });
  } else {
    const command = `launchctl enable gui/$(id -u)/${WORKER_AGENT}; ` +
      `${launchdBootstrapWithRetryCmd(WORKER_AGENT, servicePath, {
        role: "worker rollback",
        reload: false,
      })}; launchctl kickstart -k gui/$(id -u)/${WORKER_AGENT}`;
    result = await run(["bash", "-lc", command], {
      cwd: journal.sourceRoot,
      quiet: true,
    });
  }
  await checkedLifecycleCommand(result, "start restored worker");
}

export async function restoreLocalWorkerPriorLifecycle(
  journal: Readonly<LocalWorkerDeployJournal>,
): Promise<void> {
  if (!journal.priorService) return;
  if (journal.os === "linux" && journal.priorLifecycle !== "running") {
    await checkedLifecycleCommand(
      await stopLocalWorkerForActivation(journal.os, journal.sourceRoot),
      "stop temporary rollback worker",
    );
  }
  if (journal.os === "linux") {
    const policy = journal.priorStartupPolicy === "enabled"
      ? `systemctl --user enable ${WORKER_UNIT}`
      : journal.priorStartupPolicy === "masked"
        ? `systemctl --user mask --runtime ${WORKER_UNIT}`
        : `systemctl --user disable ${WORKER_UNIT} 2>/dev/null || true`;
    await checkedLifecycleCommand(
      await run(["bash", "-lc", `${SYSTEMD_RUNTIME} ${policy}`], {
        cwd: journal.sourceRoot,
        quiet: true,
      }),
      "restore worker startup policy",
    );
    return;
  }
  if (journal.priorLifecycle === "unloaded") {
    await checkedLifecycleCommand(
      await stopLocalWorkerForActivation(journal.os, journal.sourceRoot),
      "unload temporary rollback worker",
    );
  } else if (journal.priorLifecycle === "stopped") {
    await checkedLifecycleCommand(
      await run([
        "bash",
        "-lc",
        `launchctl disable gui/$(id -u)/${WORKER_AGENT}; ` +
          `launchctl stop gui/$(id -u)/${WORKER_AGENT}`,
      ], { cwd: journal.sourceRoot, quiet: true }),
      "restore loaded worker state",
    );
  }
  const override = journal.priorStartupPolicy === "disabled" ? "disable" : "enable";
  await checkedLifecycleCommand(
    await run([
      "bash",
      "-lc",
      `launchctl ${override} gui/$(id -u)/${WORKER_AGENT}`,
    ], { cwd: journal.sourceRoot, quiet: true }),
    "restore worker disabled override",
  );
}
