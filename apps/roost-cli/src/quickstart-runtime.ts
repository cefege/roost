// Quickstart runtime utilities own subprocess launch, coordinator health
// polling, and browser launch. The command orchestrator calls these after
// endpoint validation has crossed the no-effect boundary, keeping subprocess
// and secret-URL handling in one place.

import { spawn } from "bun";
import { chmodSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { registeredWorkerForGrant } from "./quickstart-bootstrap-tokens.ts";
import {
  coordinatorEnvironmentForQuickstart,
  quickstartLoopbackOrigin,
} from "./quickstart-endpoint.ts";
import type { QuickstartEndpoint } from "./quickstart-endpoint.ts";

export function logStep(message: string): void {
  console.log(`>> ${message}`);
}

export function die(message: string, ...hints: string[]): never {
  const error = new Error([message, ...hints].join(" ")) as Error & { exitCode: number };
  error.exitCode = 1;
  throw error;
}

/** Run a command with inherited stdio (user sees live output). Returns exit. */
export async function runInherit(cmd: string[], cwd?: string, env?: Record<string, string>): Promise<number> {
  const proc = spawn({
    cmd,
    cwd,
    env: env ? { ...process.env, ...env } : undefined,
    stdio: ["inherit", "inherit", "inherit"],
  });
  await proc.exited;
  return proc.exitCode ?? 1;
}

/** From-source counterpart of install-binary-agents' dry-run isolation: force
 *  BOTH the darwin (*_PLIST) and linux (*_UNIT) knobs plus the data/log dirs
 *  into a throwaway dir, so `--dry-run` can never overwrite a real service
 *  definition or restart anything. */
export async function dryRunServiceDefinitions(endpoint: QuickstartEndpoint): Promise<void> {
  const darwin = process.platform === "darwin";
  const coordDir = mkdtempSync(join(tmpdir(), "roost-dryrun-coord-"));
  const coordEnv = {
    ...coordinatorEnvironmentForQuickstart(endpoint),
    ROOST_COORD_LABEL: "com.roost.coordinator-dryrun",
    ROOST_COORD_PLIST: join(coordDir, "coord.plist"),
    ROOST_COORD_UNIT: join(coordDir, "coord.service"),
    ROOST_COORD_DATA_DIR: join(coordDir, "data"),
    ROOST_COORD_LOG_DIR: join(coordDir, "logs"),
  };
  logStep(`dry-run service definition → ${darwin ? coordEnv.ROOST_COORD_PLIST : coordEnv.ROOST_COORD_UNIT}`);
  if (await runInherit(["bash", "apps/coord/scripts/install.sh", "write-plist"], undefined, coordEnv) !== 0) {
    die("coord install.sh write-plist failed");
  }

  const workerDir = mkdtempSync(join(tmpdir(), "roost-dryrun-worker-"));
  const workerEnv = {
    ROOST_COORDINATOR_URL: endpoint.origin,
    ROOST_WORKER_AGENT_LABEL: "com.roost.worker-dryrun",
    ROOST_WORKER_PLIST: join(workerDir, "worker.plist"),
    ROOST_WORKER_UNIT: join(workerDir, "worker.service"),
    ROOST_WORKER_DATA_DIR: join(workerDir, "data"),
    ROOST_WORKER_LOG_DIR: join(workerDir, "logs"),
  };
  logStep(`dry-run service definition → ${darwin ? workerEnv.ROOST_WORKER_PLIST : workerEnv.ROOST_WORKER_UNIT}`);
  if (await runInherit(["bash", "apps/worker/scripts/install.sh", "write-plist"], undefined, workerEnv) !== 0) {
    die("worker install.sh write-plist failed");
  }
}

/** Drop an `roost` shim on PATH so `roost status` / `roost logs` work from any
 *  directory (the workspace bin isn't globally linked). Targets ~/.bun/bin —
 *  the Bun installer the curl front-door uses puts it on PATH. Returns the
 *  shim path, or null if the dir isn't on PATH (caller prints a hint). */
export function installRoostShim(repoDir: string): { path: string; onPath: boolean } | null {
  const binDir = join(homedir(), ".bun", "bin");
  try {
    mkdirSync(binDir, { recursive: true });
    const shim = join(binDir, "roost");
    writeFileSync(shim, `#!/usr/bin/env bash\nexec "${process.execPath}" "${join(repoDir, "apps/roost-cli/src/main.ts")}" "$@"\n`);
    chmodSync(shim, 0o755);
    const onPath = (process.env.PATH ?? "").split(":").includes(binDir);
    return { path: shim, onPath };
  } catch {
    return null;
  }
}

export interface QuickstartHealthDeps {
  fetch: typeof fetch;
  now: () => number;
  sleep: (ms: number) => Promise<void>;
}

const QUICKSTART_HEALTH_DEPS: QuickstartHealthDeps = {
  fetch,
  now: Date.now,
  sleep: (ms) => Bun.sleep(ms),
};

/** Poll the coordinator's own loopback listener: the operator's front door is
 * not roost's to install, so this listener is the only proof this command owns
 * that the coordinator booted. */
export async function waitForCoordHealth(
  endpoint: QuickstartEndpoint,
  timeoutMs = 15_000,
  deps: QuickstartHealthDeps = QUICKSTART_HEALTH_DEPS,
): Promise<boolean> {
  const url = `${quickstartLoopbackOrigin(endpoint)}/roost.v1.CoordinatorService/MiscHealth`;
  const deadline = deps.now() + timeoutMs;
  while (deps.now() < deadline) {
    try {
      const res = await deps.fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: "{}",
        signal: AbortSignal.timeout(3000),
      });
      if (res.ok && ((await res.json()) as { ok?: boolean }).ok === true) return true;
    } catch { /* not up yet */ }
    await deps.sleep(750);
  }
  return false;
}

export async function waitForWorkerRegistration(
  databasePath: string,
  workerToken: string,
  timeoutMs = 30_000,
): Promise<string | null> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const fingerprint = await registeredWorkerForGrant(databasePath, workerToken);
      if (fingerprint) return fingerprint;
    } catch {
      // The coordinator may briefly hold a SQLite write lock while completing
      // its first authenticated registration.
    }
    await Bun.sleep(500);
  }
  return null;
}

export type QuickstartBrowserLauncher = (command: readonly string[]) => Promise<number>;

async function launchBrowserSuppressed(command: readonly string[]): Promise<number> {
  try {
    const child = spawn([...command], {
      stdin: "ignore",
      stdout: "ignore",
      stderr: "ignore",
    });
    await child.exited;
    return child.exitCode ?? 1;
  } catch {
    return 1;
  }
}

/**
 * The complete secret URL exists only across this boundary. Opener output and
 * implementation errors are suppressed so argv echoing can never disclose it.
 */
export async function openQuickstartBrowser(
  endpoint: QuickstartEndpoint,
  browserToken: string,
  platform: NodeJS.Platform,
  launch: QuickstartBrowserLauncher = launchBrowserSuppressed,
): Promise<void> {
  const command = platform === "linux"
    ? ["xdg-open"]
    : platform === "darwin"
      ? ["open"]
      : platform === "win32"
        ? ["explorer.exe"]
        : null;
  if (!command) throw new Error("unsupported quickstart browser platform");
  const secretUrl = `${endpoint.origin}/#pair=${encodeURIComponent(browserToken)}`;
  let exit = 1;
  try {
    exit = await launch([...command, secretUrl]);
  } catch {
    // Never propagate an opener error: it may include its full argv.
  }
  if (exit !== 0) {
    throw new Error("browser opener failed; open Roost and approve a one-shot pairing request");
  }
}
