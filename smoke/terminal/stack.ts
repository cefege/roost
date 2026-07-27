import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, openSync, readFileSync, rmSync } from "node:fs";
import { once } from "node:events";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { buildAuthorizedApiClient, type AuthorizedApiClient } from "../../apps/roost-cli/src/api.ts";
const REPOSITORY_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const WORKER_LABEL = "roost-terminal-test";
const COORD_START_TIMEOUT_MS = 20_000;
const WORKER_READY_TIMEOUT_MS = 30_000;

type RunningService = {
  child: ChildProcess;
  logPath: string;
};

export type TerminalTestStack = {
  baseUrl: string;
  workerFp: string;
  coordLogPath: string;
  workerLogPath: string;
  stop(): Promise<void>;
};

function logTail(path: string): string {
  try {
    return readFileSync(path, "utf8").slice(-8_000);
  } catch {
    return "<no log output>";
  }
}

function childEnvironment(home: string, values: Record<string, string>): NodeJS.ProcessEnv {
  const env = Object.fromEntries(
    Object.entries(process.env).filter(([key, value]) => value !== undefined && !key.startsWith("ROOST_")),
  );
  return { ...env, HOME: home, ...values };
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

function keeperMatches(pid: number, socketPath: string): boolean {
  try {
    const command = execFileSync("ps", ["-wwww", "-p", String(pid), "-o", "command="], { encoding: "utf8" });
    return command.includes("multiplexed-main.ts") && command.includes(socketPath);
  } catch {
    return false;
  }
}

async function stopKeeper(workerDataDir: string): Promise<void> {
  const socketPath = join(workerDataDir, "mux-keeper.sock");
  let pid = 0;
  try { pid = Number.parseInt(readFileSync(`${socketPath}.pid`, "utf8").trim(), 10); } catch {}
  if (!Number.isSafeInteger(pid) || pid <= 0 || !keeperMatches(pid, socketPath)) return;

  try { process.kill(pid, "SIGTERM"); } catch { return; }
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    try { process.kill(pid, 0); } catch { return; }
    await delay(50);
  }
  if (!keeperMatches(pid, socketPath)) return;
  try { process.kill(pid, "SIGKILL"); } catch { return; }
  const forcedDeadline = Date.now() + 2_000;
  while (Date.now() < forcedDeadline) {
    try { process.kill(pid, 0); } catch { return; }
    await delay(50);
  }
  if (keeperMatches(pid, socketPath)) throw new Error(`keeper did not stop: pid=${pid} socket=${socketPath}`);
}

export async function startTerminalTestStack(): Promise<TerminalTestStack> {
  const root = mkdtempSync(join(tmpdir(), "roost-terminal-system-"));
  const home = join(root, "home");
  const coordLogPath = join(root, "coord.log");
  const workerLogPath = join(root, "worker.log");
  const workerDataDir = join(root, "worker-data");
  const bunExecutable = process.env.ROOST_TEST_BUN ?? "bun";
  let coord: RunningService | undefined;
  let worker: RunningService | undefined;
  let client: AuthorizedApiClient | undefined;

  const stop = async () => {
    const errors: string[] = [];
    try {
      if (client) {
        const { sessions } = await client.sessionsList({ status: "all" }).catch((error) => {
          errors.push(`list sessions: ${String(error)}`);
          return { sessions: [] };
        });
        await Promise.all(sessions.map((session) => client!.sessionsKill({ sessionId: session.id }).catch((error) => {
          errors.push(`kill session ${session.id}: ${String(error)}`);
        })));
        const { workspaces } = await client.workspacesList({}).catch((error) => {
          errors.push(`list workspaces: ${String(error)}`);
          return { workspaces: [] };
        });
        for (const workspace of workspaces) {
          for (let attempt = 0; attempt < 2; attempt++) {
            const current = await client.workspacesList({}).then((result) => result.workspaces.find((item) => item.id === workspace.id)).catch((error) => {
              errors.push(`read workspace ${workspace.id}: ${String(error)}`);
              return undefined;
            });
            if (!current) break;
            try {
              await client.workspacesDelete({ id: current.id, ifVersion: current.version });
              break;
            } catch (error) {
              if (attempt === 1) errors.push(`delete workspace ${current.id}: ${String(error)}`);
            }
          }
        }
      }
    } finally {
      await stopChild(worker).catch((error) => errors.push(`stop worker: ${String(error)}`));
      await stopKeeper(workerDataDir).catch((error) => errors.push(`stop keeper: ${String(error)}`));
      await stopChild(coord).catch((error) => errors.push(`stop coordinator: ${String(error)}`));
      try { rmSync(root, { recursive: true, force: true }); } catch (error) { errors.push(`remove test root: ${String(error)}`); }
    }
    if (errors.length > 0) throw new Error(`terminal stack cleanup failed:\n${errors.join("\n")}`);
  };

  try {
    const coordLog = openSync(coordLogPath, "a");
    coord = {
      logPath: coordLogPath,
      child: spawn(bunExecutable, ["apps/coord/src/main.ts"], {
        cwd: REPOSITORY_ROOT,
        env: childEnvironment(home, {
          ROOST_COORDINATOR_BIND: "127.0.0.1:0",
          ROOST_COORDINATOR_DB: join(root, "coord.db"),
          ROOST_COORDINATOR_AUTHORIZED_KEYS: join(root, "authorized_keys.roost"),
          ROOST_COORDINATOR_KEY_PATH: join(root, "coord.key"),
          ROOST_WEB_DIST_PATH: join(REPOSITORY_ROOT, "apps/web/dist"),
        }),
        stdio: ["ignore", coordLog, coordLog],
      }),
    };
    const baseUrl = await waitFor("coordinator startup", COORD_START_TIMEOUT_MS, () => {
      const match = /"msg":"listening"[^\n]*"bind":"([^"]+)"/.exec(logTail(coordLogPath));
      return match ? `http://${match[1]}` : undefined;
    }).catch((error) => { throw new Error(`${error}\ncoord log:\n${logTail(coordLogPath)}`); });

    client = await buildAuthorizedApiClient({
      coordinatorUrl: baseUrl,
      keyPath: join(root, "api.key"),
      label: "roost-terminal-test-api",
    });
    const bootstrapToken = (await client.authMintBootstrap({ kind: "worker", label: WORKER_LABEL })).token;

    const workerLog = openSync(workerLogPath, "a");
    worker = {
      logPath: workerLogPath,
      child: spawn(bunExecutable, ["apps/worker/src/main.ts"], {
        cwd: REPOSITORY_ROOT,
        env: childEnvironment(home, {
          ROOST_COORDINATOR_URL: baseUrl,
          ROOST_BOOTSTRAP_TOKEN: bootstrapToken,
          ROOST_WORKER_LABEL: WORKER_LABEL,
          ROOST_WORKER_DATA_DIR: workerDataDir,
          ROOST_WORKER_KEY_PATH: join(workerDataDir, "worker.key"),
          ROOST_KEEPER_QUIET: "1",
        }),
        stdio: ["ignore", workerLog, workerLog],
      }),
    };
    const workerFp = await waitFor("worker routable", WORKER_READY_TIMEOUT_MS, async () => {
      const result = await client!.workersList({});
      const candidate = result.workers.find((item) => item.label === WORKER_LABEL);
      return candidate && result.routableFps.includes(candidate.fp) ? candidate.fp : undefined;
    }).catch((error) => { throw new Error(`${error}\nworker log:\n${logTail(workerLogPath)}`); });

    return { baseUrl, workerFp, coordLogPath, workerLogPath, stop };
  } catch (error) {
    const logs = `coord log:\n${logTail(coordLogPath)}\nworker log:\n${logTail(workerLogPath)}`;
    await stop().catch(() => undefined);
    throw new Error(`${String(error)}\n${logs}`);
  }
}
