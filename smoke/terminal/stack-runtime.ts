// Terminal smoke stack support owns child environments, the coordinator launch,
// test API-key authorization, and teardown. The stack lifecycle calls these
// helpers while retaining ownership of spawned services, and an upgrade run
// relaunches the coordinator from a second checkout through the same launcher.
// Keeping process cleanup and key authorization together prevents hermetic stacks leaking state.

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { openSync, readFileSync } from "node:fs";
import { once } from "node:events";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { resolveLocalEndpoint } from "../../apps/shared/src/local-endpoint.ts";
import { shutdownKeeperAuthenticated } from "../../apps/worker/src/keeper/keeper-probe.ts";

export const REPOSITORY_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

export type RunningService = {
  child: ChildProcess;
  logPath: string;
};
function logTail(path: string): string {
  try {
    return readFileSync(path, "utf8").slice(-8_000);
  } catch {
    return "<no log output>";
  }
}

function childEnvironment(home: string, tmpDir: string, values: Record<string, string>): NodeJS.ProcessEnv {
  const env = Object.fromEntries(
    Object.entries(process.env).filter(([key, value]) => value !== undefined && !key.startsWith("ROOST_")),
  );
  // Every child gets its own temp namespace. apps/worker/src/shell-spec.ts
  // materializes the POSIX bootstrap rc at a FIXED tmpdir() path
  // (roost-bash-osc7/roost.bashrc, roost-zsh-noPROMPT_SP/.zshrc), once per
  // process, with a truncating write: any two workers sharing a temp root race
  // there, and a shell that sources the file mid-truncate silently loses its

  // OSC7 cwd tracking. That is reachable both across concurrent stacks (one
  // per Playwright worker) and inside one stack, whose primary and second
  // workers are separate processes. TMP/TEMP carry the same isolation on
  // Windows, where os.tmpdir() reads those instead of TMPDIR.
  return { ...env, HOME: home, TMPDIR: tmpDir, TMP: tmpDir, TEMP: tmpDir, ...values };
}
/**
 * Authorize the harness API key against the coordinator's sole self-hosted
 * account. The coordinator created that account while validating its startup
 * invariant, so the fixture adopts the row that exists: inserting a topology of
 * its own is exactly what that invariant rejects on the next boot.
 * Runs in a `bun -e` child because the Playwright runner has no bun:sqlite.
 */
function authorizeTerminalTestApiKey(
  bunExecutable: string,
  dbPath: string,
  deviceFingerprint: string,
  publicKey: Uint8Array,
): void {
  const script = `
    import { Database } from "bun:sqlite";
    const db = new Database(process.env.ROOST_TERMINAL_DB);
    const now = Number(process.env.ROOST_TERMINAL_NOW);
    const key = Buffer.from(process.env.ROOST_TERMINAL_PUBLIC_KEY, "base64");
    const fp = process.env.ROOST_TERMINAL_DEVICE_FP;
    try {
      db.exec("BEGIN IMMEDIATE");
      const accounts = db.query("SELECT id FROM accounts").all();
      if (accounts.length !== 1) {
        throw new Error("expected one self-hosted account, found " + accounts.length);
      }
      db.query("INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) VALUES (?, ?, ?, ?)").run(fp, key, "roost-terminal-test-api", now);
      db.query("INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) VALUES (?, ?, ?, ?)").run(fp, accounts[0].id, now, now);
      db.exec("COMMIT");
    } catch (error) {
      try { db.exec("ROLLBACK"); } catch {}
      throw error;
    } finally {
      db.close();
    }
  `;
  execFileSync(bunExecutable, ["-e", script], {
    cwd: REPOSITORY_ROOT,
    env: {
      ...process.env,
      ROOST_TERMINAL_DB: dbPath,
      ROOST_TERMINAL_NOW: String(Date.now()),
      ROOST_TERMINAL_PUBLIC_KEY: Buffer.from(publicKey).toString("base64"),
      ROOST_TERMINAL_DEVICE_FP: deviceFingerprint,
    },
  });
}

async function waitFor<T>(label: string, timeoutMs: number, probe: () => T | undefined | Promise<T | undefined>): Promise<T> {

  const deadline = Date.now() + timeoutMs;
  let lastError: unknown;
  while (Date.now() < deadline) {
    try {
      const result = await probe();
      if (result !== undefined) return result;
      lastError = undefined;
    } catch (error) {
      lastError = error;
    }
    await delay(100);
  }
  throw new Error(`${label} timed out after ${timeoutMs}ms${lastError ? `: ${String(lastError)}` : ""}`);
}
async function stopChild(service: RunningService | undefined): Promise<void> {
  if (!service || service.child.exitCode !== null || service.child.killed) return;
  service.child.kill("SIGTERM");
  const graceful = await Promise.race([
    once(service.child, "exit").then(() => true),
    delay(5_000).then(() => false),
  ]);
  if (!graceful && service.child.exitCode === null) {
    service.child.kill("SIGKILL");
    await Promise.race([once(service.child, "exit"), delay(2_000)]);
  }
}

async function stopKeeper(workerDataDir: string): Promise<void> {
  await shutdownKeeperAuthenticated(resolveLocalEndpoint({
    name: "mux-keeper",
    dataDir: workerDataDir,
  }));
}

/** Stop a worker a deploy left running: it is not a child of this process, so
 *  only its pid is available and liveness is polled with signal 0. */
async function stopDeployedWorker(pid: number | undefined): Promise<void> {
  if (pid === undefined) return;
  try {
    process.kill(pid, "SIGTERM");
  } catch {
    return;
  }
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    try {
      process.kill(pid, 0);
    } catch {
      return;
    }
    await delay(100);
  }
  try {
    process.kill(pid, "SIGKILL");
  } catch {
    // Exited between the liveness probe and the hard kill.
  }
}

export interface CoordinatorServiceConfig {
  bunExecutable: string;
  /** Checkout the coordinator process runs from; a release upgrade swaps it. */
  sourceRoot: string;
  root: string;
  home: string;
  tmpDir: string;
  bind: string;
  dbPath: string;
  logPath: string;
  gitSha: string;
}

export function startCoordinatorService(config: CoordinatorServiceConfig): RunningService {
  const coordLog = openSync(config.logPath, "a");
  return {
    logPath: config.logPath,
    child: spawn(config.bunExecutable, ["apps/coord/src/main.ts"], {
      cwd: config.sourceRoot,
      env: childEnvironment(config.home, config.tmpDir, {
        ROOST_COORDINATOR_BIND: config.bind,
        // Bun auto-loads the checkout's .env after spawn, so the hermetic
        // loopback auth semantics are pinned explicitly rather than inherited.
        ROOST_TRUST_PROXY: "0",
        ROOST_RELAXED_CSP: "1",
        ROOST_COORDINATOR_DB: config.dbPath,
        ROOST_COORDINATOR_AUTHORIZED_KEYS: join(config.root, "authorized_keys.roost"),
        // The SPA is always the working tree's build: apps/web/dist is not
        // committed, so a prior-release checkout has none to serve.
        ROOST_WEB_DIST_PATH: join(REPOSITORY_ROOT, "apps/web/dist"),
        ROOST_GIT_SHA: config.gitSha,
      }),
      stdio: ["ignore", coordLog, coordLog],
    }),
  };
}


export {
  authorizeTerminalTestApiKey,
  childEnvironment,
  logTail,
  stopChild,
  stopDeployedWorker,
  stopKeeper,
  waitFor,
};
