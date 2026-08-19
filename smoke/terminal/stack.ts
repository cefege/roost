import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { mkdirSync, mkdtempSync, openSync, readFileSync, realpathSync, rmSync } from "node:fs";
import { once } from "node:events";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { buildAuthorizedApiClient, type AuthorizedApiClient } from "../../apps/roost-cli/src/api.ts";
import { resolveLocalEndpoint } from "../../apps/shared/src/local-endpoint.ts";
import { shutdownKeeperAuthenticated } from "../../apps/worker/src/keeper/keeper-probe.ts";
const REPOSITORY_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const WORKER_LABEL = "roost-terminal-test";
const SECOND_WORKER_LABEL = "roost-terminal-test-second";
const PTY_FIXTURE_WORKER_LABEL = "roost-terminal-test-pty-fixture";
const COORD_START_TIMEOUT_MS = 20_000;
const WORKER_READY_TIMEOUT_MS = 30_000;

type RunningService = {
  child: ChildProcess;
  logPath: string;
};

export type TerminalTestWorker = {
  workerFp: string;
  label: string;
  home: string;
  logPath: string;
};

export type TerminalTestStack = {
  baseUrl: string;
  workerFp: string;
  workerHome: string;
  coordLogPath: string;
  workerLogPath: string;
  ptyFixtureWorkerLogPath: string;
  secondWorkerLogPath: string;
  // The authorized client the harness already had to mint to bootstrap the
  // worker. Exposed so callers don't build a second (unauthorized) one.
  client: AuthorizedApiClient;
  // Lazily start one independent worker with its own HOME, data, key, log, and
  // keeper. Repeated calls return the same running worker.
  startSecondWorker(): Promise<TerminalTestWorker>;
  /** Lazily start a worker whose shell is the compiled portable PTY fixture. */
  startPtyFixtureWorker(): Promise<TerminalTestWorker>;
  // Bounce the primary worker process, keeping coord and the persisted worker
  // identity. Resolves once the same fingerprint is routable again.
  restartWorker(): Promise<void>;
  stop(): Promise<void>;
};

export type TerminalTestStackOptions = {
  // Keep the caller's real HOME instead of the isolated temp one. Needed only
  // by the agent smoke: the worker forks `omp`, which reads its model
  // credentials from the real ~/.omp — under a temp HOME every turn fails
  // unauthenticated. Coord/worker state stays isolated either way (their paths
  // are ROOST_* env overrides, not HOME-derived).
  useRealHome?: boolean;
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

export async function startTerminalTestStack(
  options: TerminalTestStackOptions = {},
): Promise<TerminalTestStack> {
  // AF_UNIX sun_path caps at 104 bytes on macOS, and os.tmpdir() there is
  // /var/folders/<xx>/<hash>/T (~50 chars, ~58 once realpath adds /private) —
  // long enough that the worker's <home>/.roost/agent-report.sock overflowed the
  // limit with "OSError: AF_UNIX path too long". /tmp keeps the whole tree short.
  // realpathSync: macOS tmp dirs are symlinks and workers report resolved cwds.
  const tmpRoot = process.platform === "win32" ? tmpdir() : "/tmp";
  const root = realpathSync(mkdtempSync(join(tmpRoot, "roost-terminal-system-")));
  const home = options.useRealHome ? (process.env.HOME ?? join(root, "home")) : join(root, "home");
  const secondHome = join(root, "second-home");
  const coordLogPath = join(root, "coord.log");
  const workerLogPath = join(root, "worker.log");
  const secondWorkerLogPath = join(root, "second-worker.log");
  const ptyFixtureHome = join(root, "pty-fixture-home");
  const ptyFixtureLogPath = join(root, "pty-fixture-worker.log");
  const ptyFixtureDataDir = join(root, "pty-fixture-worker-data");
  const ptyFixtureExecutable = join(
    root,
    process.platform === "win32" ? "roost-pty-fixture.exe" : "roost-pty-fixture",
  );
  const workerDataDir = join(root, "worker-data");
  const secondWorkerDataDir = join(root, "second-worker-data");
  const bunExecutable = process.env.ROOST_TEST_BUN ?? "bun";
  mkdirSync(home, { recursive: true });
  mkdirSync(secondHome, { recursive: true });
  mkdirSync(ptyFixtureHome, { recursive: true });
  const childTmpDirs = {
    coord: join(root, "coord-tmp"),
    worker: join(root, "worker-tmp"),
    secondWorker: join(root, "second-worker-tmp"),
    ptyFixtureWorker: join(root, "pty-fixture-worker-tmp"),
  };
  for (const dir of Object.values(childTmpDirs)) mkdirSync(dir, { recursive: true });
  let coord: RunningService | undefined;
  let worker: RunningService | undefined;
  let secondWorker: RunningService | undefined;
  let secondWorkerStart: Promise<TerminalTestWorker> | undefined;
  let ptyFixtureWorker: RunningService | undefined;
  let ptyFixtureWorkerStart: Promise<TerminalTestWorker> | undefined;
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
      await stopChild(secondWorker).catch((error) => errors.push(`stop second worker: ${String(error)}`));
      await stopChild(ptyFixtureWorker).catch((error) => errors.push(`stop PTY fixture worker: ${String(error)}`));
      await stopKeeper(ptyFixtureDataDir).catch((error) => errors.push(`stop PTY fixture keeper: ${String(error)}`));
      await stopKeeper(secondWorkerDataDir).catch((error) => errors.push(`stop second keeper: ${String(error)}`));
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
        env: childEnvironment(home, childTmpDirs.coord, {
          ROOST_COORDINATOR_BIND: "127.0.0.1:0",
          // Bun auto-loads the repository .env after process spawn. Explicit
          // overrides keep the hermetic listener on loopback auth semantics and
          // disable production's secondary Cloudflare listener.
          ROOST_TRUST_PROXY: "0",
          ROOST_PUBLIC_BIND: "",
          ROOST_RELAXED_CSP: "1",
          ROOST_COORDINATOR_DB: join(root, "coord.db"),
          ROOST_COORDINATOR_AUTHORIZED_KEYS: join(root, "authorized_keys.roost"),
          ROOST_COORDINATOR_KEY_PATH: join(root, "coord.key"),
          // Isolate the relocation state too. It defaults under the data dir
          // (HOME-derived), so a caller running with useRealHome would
          // otherwise inherit a real "coordinator relocated" handoff and the
          // test coord would 410 every non-GET request.
          ROOST_COORDINATOR_HANDOFF_PATH: join(root, "coord-handoff.json"),
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
    const startWorker = (config: {
      label: string;
      home: string;
      logPath: string;
      dataDir: string;
      tmpDir: string;
      bootstrapToken: string;
      shell?: string;
    }): RunningService => {
      const workerLog = openSync(config.logPath, "a");
      return {
        logPath: config.logPath,
        child: spawn(bunExecutable, ["apps/worker/src/main.ts"], {
          cwd: REPOSITORY_ROOT,
          env: childEnvironment(config.home, config.tmpDir, {
            ROOST_COORDINATOR_URL: baseUrl,
            // Only the first boot redeems the token; persisted data owns the
            // identity on restart.
            ROOST_BOOTSTRAP_TOKEN: config.bootstrapToken,
            ROOST_WORKER_LABEL: config.label,
            ROOST_WORKER_DATA_DIR: config.dataDir,
            ROOST_WORKER_KEY_PATH: join(config.dataDir, "worker.key"),
            ROOST_KEEPER_QUIET: "1",
            ...(config.shell ? { SHELL: config.shell, ROOST_SHELL: config.shell } : {}),
          }),
          stdio: ["ignore", workerLog, workerLog],
        }),
      };
    };
    const awaitWorkerRoutable = (label: string, logPath: string) =>
      waitFor(`${label} routable`, WORKER_READY_TIMEOUT_MS, async () => {
        const result = await client!.workersList({});
        const candidate = result.workers.find((item) => item.label === label);
        return candidate && result.routableFps.includes(candidate.fp) ? candidate.fp : undefined;
      }).catch((error) => { throw new Error(`${error}\nworker log:\n${logTail(logPath)}`); });

    const bootstrapToken = (await client.authMintBootstrap({ kind: "worker", label: WORKER_LABEL })).token;
    worker = startWorker({
      label: WORKER_LABEL,
      home,
      logPath: workerLogPath,
      dataDir: workerDataDir,
      tmpDir: childTmpDirs.worker,
      bootstrapToken,
    });
    const workerFp = await awaitWorkerRoutable(WORKER_LABEL, workerLogPath);

    const startSecondWorker = (): Promise<TerminalTestWorker> => {
      secondWorkerStart ??= (async () => {
        const secondBootstrapToken = (
          await client!.authMintBootstrap({ kind: "worker", label: SECOND_WORKER_LABEL })
        ).token;
        secondWorker = startWorker({
          label: SECOND_WORKER_LABEL,
          home: secondHome,
          logPath: secondWorkerLogPath,
          dataDir: secondWorkerDataDir,
          tmpDir: childTmpDirs.secondWorker,
          bootstrapToken: secondBootstrapToken,
        });
        const workerFp = await awaitWorkerRoutable(SECOND_WORKER_LABEL, secondWorkerLogPath);
        return { workerFp, label: SECOND_WORKER_LABEL, home: secondHome, logPath: secondWorkerLogPath };
      })();
      return secondWorkerStart;
    };
    const startPtyFixtureWorker = (): Promise<TerminalTestWorker> => {
      ptyFixtureWorkerStart ??= (async () => {
        execFileSync(
          bunExecutable,
          [
            "build",
            "--compile",
            join(REPOSITORY_ROOT, "smoke", "terminal", "pty-fixture.ts"),
            "--outfile",
            ptyFixtureExecutable,
          ],
          { cwd: REPOSITORY_ROOT, stdio: "pipe" },
        );
        const fixtureBootstrapToken = (
          await client!.authMintBootstrap({ kind: "worker", label: PTY_FIXTURE_WORKER_LABEL })
        ).token;
        ptyFixtureWorker = startWorker({
          label: PTY_FIXTURE_WORKER_LABEL,
          home: ptyFixtureHome,
          logPath: ptyFixtureLogPath,
          dataDir: ptyFixtureDataDir,
          tmpDir: childTmpDirs.ptyFixtureWorker,
          bootstrapToken: fixtureBootstrapToken,
          shell: ptyFixtureExecutable,
        });
        const workerFp = await awaitWorkerRoutable(PTY_FIXTURE_WORKER_LABEL, ptyFixtureLogPath);
        return {
          workerFp,
          label: PTY_FIXTURE_WORKER_LABEL,
          home: ptyFixtureHome,
          logPath: ptyFixtureLogPath,
        };
      })();
      return ptyFixtureWorkerStart;
    };


    // Full primary-worker bounce, keeping coord and the persisted identity.
    // The keeper is deliberately left alone: it is designed to outlive the
    // worker, and agent sessions never touch it anyway.
    const restartWorker = async () => {
      await stopChild(worker);
      worker = startWorker({
        label: WORKER_LABEL,
        home,
        logPath: workerLogPath,
        dataDir: workerDataDir,
        tmpDir: childTmpDirs.worker,
        bootstrapToken,
      });
      await awaitWorkerRoutable(WORKER_LABEL, workerLogPath);
    };

    return {
      baseUrl,
      workerFp,
      workerHome: home,
      coordLogPath,
      workerLogPath,
      secondWorkerLogPath,
      ptyFixtureWorkerLogPath: ptyFixtureLogPath,
      client,
      startSecondWorker,
      startPtyFixtureWorker,
      restartWorker,
      stop,
    };
  } catch (error) {
    const logs = `coord log:\n${logTail(coordLogPath)}\nworker log:\n${logTail(workerLogPath)}\nsecond worker log:\n${logTail(secondWorkerLogPath)}`;
    await stop().catch(() => undefined);
    throw new Error(`${String(error)}\n${logs}`);
  }
}
