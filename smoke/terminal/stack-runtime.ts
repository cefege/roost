// Terminal smoke stack support owns child environments, dashboard seeding, and teardown.
// The stack lifecycle calls these helpers while retaining ownership of spawned services.
// Keeping process cleanup and authorization scoping together prevents hermetic stacks leaking state.

import { execFileSync, type ChildProcess } from "node:child_process";
import { readFileSync } from "node:fs";
import { once } from "node:events";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import type { AuthorizedApiClient } from "../../apps/roost-cli/src/api.ts";
import { X_ROOST_DASHBOARD_ID } from "../../apps/shared/src/wire/headers.ts";
import { resolveLocalEndpoint } from "../../apps/shared/src/local-endpoint.ts";
import { shutdownKeeperAuthenticated } from "../../apps/worker/src/keeper/keeper-probe.ts";

export const REPOSITORY_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const TERMINAL_TEST_ACCOUNT_ID = "terminal-test-account";
const TERMINAL_TEST_ORGANIZATION_ID = "terminal-test-organization";
export const TERMINAL_TEST_DASHBOARD_ID = "terminal-test-dashboard";
export const TERMINAL_TEST_SECOND_DASHBOARD_ID = "terminal-test-dashboard-b";

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
function seedTerminalDashboards(
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
    const account = process.env.ROOST_TERMINAL_ACCOUNT;
    const organization = process.env.ROOST_TERMINAL_ORGANIZATION;
    const dashboard = process.env.ROOST_TERMINAL_DASHBOARD;
    const secondDashboard = process.env.ROOST_TERMINAL_SECOND_DASHBOARD;
    try {
      db.exec("BEGIN IMMEDIATE");
      db.query("INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) VALUES (?, ?, ?, ?)").run(fp, key, "roost-terminal-test-api", now);
      db.query("INSERT INTO accounts (id, email_normalized, password_hash, status, created_at_ms, password_changed_at_ms) VALUES (?, ?, NULL, 'active', ?, NULL)").run(account, "terminal-smoke@example.test", now);
      db.query("INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) VALUES (?, ?, ?, ?)").run(fp, account, now, now);
      db.query("INSERT INTO organizations (id, slug, name, status, created_at_ms) VALUES (?, ?, ?, 'active', ?)").run(organization, organization, "Terminal Test Organization", now);
      db.query("INSERT INTO organization_memberships (organization_id, account_id, role, created_at_ms) VALUES (?, ?, 'owner', ?)").run(organization, account, now);
      db.query("INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms) VALUES (?, ?, ?, ?, 'active', ?)").run(dashboard, organization, dashboard, "Terminal Test Dashboard", now);
      db.query("INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms) VALUES (?, ?, ?, ?, 'active', ?)").run(secondDashboard, organization, secondDashboard, "Terminal Test Dashboard B", now);
      db.query("INSERT INTO dashboard_memberships (dashboard_id, account_id, role, created_at_ms) VALUES (?, ?, 'admin', ?)").run(dashboard, account, now);
      db.query("INSERT INTO dashboard_memberships (dashboard_id, account_id, role, created_at_ms) VALUES (?, ?, 'admin', ?)").run(secondDashboard, account, now);
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
      ROOST_TERMINAL_ACCOUNT: TERMINAL_TEST_ACCOUNT_ID,
      ROOST_TERMINAL_ORGANIZATION: TERMINAL_TEST_ORGANIZATION_ID,
      ROOST_TERMINAL_DASHBOARD: TERMINAL_TEST_DASHBOARD_ID,
      ROOST_TERMINAL_SECOND_DASHBOARD: TERMINAL_TEST_SECOND_DASHBOARD_ID,
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
function isCallOptions(value: unknown): value is { headers?: HeadersInit } {
  return typeof value === "object" && value !== null;
}

function withTerminalDashboard(
  client: AuthorizedApiClient,
  dashboardId: string,
): AuthorizedApiClient {
  return new Proxy(client, {
    get(target, property, receiver) {
      const value = Reflect.get(target, property, receiver);
      if (typeof value !== "function") return value;
      return (...args: unknown[]) => {
        const [request, callOptions] = args;
        const options = isCallOptions(callOptions) ? callOptions : {};
        const headers = new Headers(options.headers);
        headers.set(X_ROOST_DASHBOARD_ID, dashboardId);
        return Reflect.apply(value, target, [request, { ...options, headers }]);
      };
    },
  }) as AuthorizedApiClient;
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


export {
  childEnvironment,
  logTail,
  seedTerminalDashboards,
  stopChild,
  stopKeeper,
  waitFor,
  withTerminalDashboard,
};
