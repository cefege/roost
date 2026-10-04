// Terminal smoke stack support owns child environments, the coordinator launch,
// test API-key authorization, and teardown. The stack lifecycle calls these
// helpers while retaining ownership of spawned services.
// Keeping process cleanup and key authorization together prevents hermetic stacks leaking state.

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, openSync, readFileSync } from "node:fs";
import { once } from "node:events";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import type { AuthorizedApiClient } from "../support/coord-client.ts";

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
  // Every child gets its own temp namespace. The worker materializes the POSIX
  // shell bootstrap rc at a FIXED path under its temp root, once per process,
  // with a truncating write: any two workers sharing a temp root race there,
  // and a shell that sources the file mid-truncate silently loses its OSC7 cwd
  // tracking. That is reachable both across concurrent stacks (one
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
    // The coordinator is already live on this file and takes short write locks
    // of its own, so an unqualified BEGIN IMMEDIATE races it. Wait the lock out
    // rather than failing the stack on a contended box.
    db.exec("PRAGMA busy_timeout=10000");
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

/**
 * Stop the keeper a worker spawned. The keeper outlives its worker by design
 * and is not a child of this process, so it is found through the pid file the
 * worker keeps beside `mux-keeper.sock` in its data directory, and liveness is
 * polled with signal 0. `roost-keeper` exits on SIGTERM.
 */
async function stopKeeper(workerDataDir: string): Promise<void> {
  const pidPath = join(workerDataDir, "mux-keeper.pid");
  if (!existsSync(pidPath)) return;
  const pid = Number.parseInt(readFileSync(pidPath, "utf8").trim(), 10);
  if (!Number.isSafeInteger(pid) || pid <= 0) return;
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

/**
 * Delete every session and workspace the coordinator still holds, the way an
 * uninstall would, collecting each refusal instead of stopping at the first.
 *
 * The stack owns this rather than a spec because rows left behind make the
 * NEXT spec's row assertions depend on spec order, and the failure then names
 * the wrong test. A version mismatch is retried against a fresh read: the
 * delete is conditional on the version it was handed, so a concurrent write
 * makes the first attempt fail without anything being wrong.
 */
export async function cleanInstallResources(
  installClient: AuthorizedApiClient,
  errors: string[],
): Promise<void> {
  const { sessions } = await installClient.sessionsList({ status: "all" }).catch((error) => {
    errors.push(`list sessions: ${String(error)}`);
    return { sessions: [] };
  });
  await Promise.all(sessions.map((session) => installClient.sessionsKill({ sessionId: session.id }).catch((error) => {
    errors.push(`kill session ${session.id}: ${String(error)}`);
  })));
  const { workspaces } = await installClient.workspacesList({}).catch((error) => {
    errors.push(`list workspaces: ${String(error)}`);
    return { workspaces: [] };
  });
  for (const workspace of workspaces) {
    for (let attempt = 0; attempt < 2; attempt++) {
      const current = await installClient.workspacesList({}).then((result) =>
        result.workspaces.find((item) => item.id === workspace.id),
      ).catch((error) => {
        errors.push(`read workspace ${workspace.id}: ${String(error)}`);
        return undefined;
      });
      if (!current) break;
      try {
        await installClient.workspacesDelete({ id: current.id, ifVersion: current.version });
        break;
      } catch (error) {
        if (attempt === 1) errors.push(`delete workspace ${current.id}: ${String(error)}`);
      }
    }
  }
}

export interface CoordinatorServiceConfig {
  /** The `roost` binary; it receives the ordinary `coord` subcommand. */
  coordExecutable: string;
  /** The dx bundle the coordinator serves. */
  webDist: string;
  root: string;
  home: string;
  tmpDir: string;
  bind: string;
  dbPath: string;
  logPath: string;
  /** Extra origins the coordinator accepts cross-origin RPC and Sync upgrades
   *  from; the harness's worker-served local UI origins are not the 4104
   *  default the product pre-allowlists. */
  corsAllowedOrigins?: readonly string[];
  /** Explicit CSP profile; false must reach config as the literal string "0". */
  relaxedCsp?: boolean;
  /** Explicit public endpoint values; empty strings clear auto-loaded endpoint settings. */
  webPublicUrl?: string;
  coordinatorPublicUrl?: string;
  /** Explicit peer enablement for a hermetic stack; absent preserves product defaults. */
  terminalPeerEnabled?: boolean;
  /** Explicitly empty disables STUN discovery without changing coordinator behavior. */
  terminalPeerStunUrls?: readonly string[];
  gitSha: string;
}

export function startCoordinatorService(config: CoordinatorServiceConfig): RunningService {
  const coordLog = openSync(config.logPath, "a");
  return {
    logPath: config.logPath,
    child: spawn(config.coordExecutable, ["coord"], {
      cwd: REPOSITORY_ROOT,
      env: childEnvironment(config.home, config.tmpDir, {
        ROOST_COORDINATOR_BIND: config.bind,
        // The hermetic loopback auth semantics are pinned explicitly rather
        // than inherited from whatever the caller's shell exported.
        ROOST_TRUST_PROXY: "0",
        ROOST_RELAXED_CSP: config.relaxedCsp === false ? "0" : "1",
        ROOST_COORDINATOR_DB: config.dbPath,
        ROOST_COORDINATOR_AUTHORIZED_KEYS: join(config.root, "authorized_keys.roost"),
        ROOST_WEB_DIST_PATH: config.webDist,
        ROOST_GIT_SHA: config.gitSha,
        ...(config.webPublicUrl === undefined ? {} : { ROOST_WEB_PUBLIC_URL: config.webPublicUrl }),
        ...(config.coordinatorPublicUrl === undefined
          ? {}
          : { ROOST_COORDINATOR_PUBLIC_URL: config.coordinatorPublicUrl }),
        ...(config.corsAllowedOrigins?.length
          ? { ROOST_CORS_ALLOWED_ORIGINS: config.corsAllowedOrigins.join(",") }
          : {}),
        ...(config.terminalPeerEnabled === undefined
          ? {}
          : { ROOST_TERMINAL_PEER_ENABLED: config.terminalPeerEnabled ? "1" : "0" }),
        ...(config.terminalPeerStunUrls === undefined
          ? {}
          : { ROOST_TERMINAL_PEER_STUN_URLS: config.terminalPeerStunUrls.join(",") }),
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
  stopKeeper,
  waitFor,
};
