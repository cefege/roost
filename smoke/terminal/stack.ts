// The terminal smoke stack starts an isolated coordinator, workers, keepers, and API scope.
// Playwright fixtures call this lifecycle and receive lazy worker factories plus cleanup.
// Every child gets isolated state and temp roots while the returned stop closes all resources.

import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, openSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildAuthorizedApiClient, type AuthorizedApiClient } from "../../apps/roost-cli/src/api.ts";
import { loadWorkerKey } from "../../apps/worker/src/jwt.ts";
import {
  REPOSITORY_ROOT,
  TERMINAL_TEST_DASHBOARD_ID,
  TERMINAL_TEST_SECOND_DASHBOARD_ID,
  childEnvironment,
  logTail,
  seedTerminalDashboards,
  stopChild,
  stopKeeper,
  waitFor,
  withTerminalDashboard,
  type RunningService,
} from "./stack-runtime.ts";
import {
  createPtyFixtureCompiler,
  createTerminalWorkerStarter,
  waitForTerminalWorkerRoutable,
} from "./stack-worker-runtime.ts";
const WORKER_LABEL = "roost-terminal-test";
const SECOND_WORKER_LABEL = "roost-terminal-test-second";
const PTY_FIXTURE_WORKER_LABEL = "roost-terminal-test-pty-fixture";
const COORD_START_TIMEOUT_MS = 20_000;
const SECOND_DASHBOARD_PTY_FIXTURE_WORKER_LABEL = "roost-terminal-test-dashboard-b-pty-fixture";

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
  secondDashboardPtyFixtureWorkerLogPath: string;
  // The authorized client the harness already had to mint to bootstrap the
  // worker. Exposed so callers don't build a second (unauthorized) one.
  dashboardId: string;
  secondDashboardId: string;
  secondDashboardClient: AuthorizedApiClient;
  client: AuthorizedApiClient;
  // Lazily start one independent worker with its own HOME, data, key, log, and
  // keeper. Repeated calls return the same running worker.
  startSecondWorker(): Promise<TerminalTestWorker>;
  /** Lazily start a worker whose shell is the compiled portable PTY fixture. */
  startPtyFixtureWorker(): Promise<TerminalTestWorker>;
  /** Lazily start the portable PTY fixture worker bound to the second dashboard. */
  startSecondDashboardPtyFixtureWorker(): Promise<TerminalTestWorker>;
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
  const coordDbPath = join(root, "coord.db");
  const workerLogPath = join(root, "worker.log");
  const secondWorkerLogPath = join(root, "second-worker.log");
  const ptyFixtureHome = join(root, "pty-fixture-home");
  const ptyFixtureLogPath = join(root, "pty-fixture-worker.log");
  const ptyFixtureDataDir = join(root, "pty-fixture-worker-data");
  const secondDashboardPtyFixtureHome = join(root, "dashboard-b-pty-fixture-home");
  const secondDashboardPtyFixtureLogPath = join(root, "dashboard-b-pty-fixture-worker.log");
  const secondDashboardPtyFixtureDataDir = join(root, "dashboard-b-pty-fixture-worker-data");
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
  mkdirSync(secondDashboardPtyFixtureHome, { recursive: true });
  const childTmpDirs = {
    coord: join(root, "coord-tmp"),
    worker: join(root, "worker-tmp"),
    secondWorker: join(root, "second-worker-tmp"),
    ptyFixtureWorker: join(root, "pty-fixture-worker-tmp"),
    secondDashboardPtyFixtureWorker: join(root, "dashboard-b-pty-fixture-worker-tmp"),
  };
  for (const dir of Object.values(childTmpDirs)) mkdirSync(dir, { recursive: true });
  let coord: RunningService | undefined;
  let worker: RunningService | undefined;
  let secondWorker: RunningService | undefined;
  let secondWorkerStart: Promise<TerminalTestWorker> | undefined;
  let ptyFixtureWorker: RunningService | undefined;
  let ptyFixtureWorkerStart: Promise<TerminalTestWorker> | undefined;
  let secondDashboardPtyFixtureWorker: RunningService | undefined;
  let secondDashboardPtyFixtureWorkerStart: Promise<TerminalTestWorker> | undefined;
  let client: AuthorizedApiClient | undefined;
  let secondDashboardClient: AuthorizedApiClient | undefined;

  const stop = async () => {
    const errors: string[] = [];
    try {
      const cleanDashboard = async (scopeClient: AuthorizedApiClient, scopeName: string): Promise<void> => {
        const { sessions } = await scopeClient.sessionsList({ status: "all" }).catch((error) => {
          errors.push(`${scopeName}: list sessions: ${String(error)}`);
          return { sessions: [] };
        });
        await Promise.all(sessions.map((session) => scopeClient.sessionsKill({ sessionId: session.id }).catch((error) => {
          errors.push(`${scopeName}: kill session ${session.id}: ${String(error)}`);
        })));
        const { workspaces } = await scopeClient.workspacesList({}).catch((error) => {
          errors.push(`${scopeName}: list workspaces: ${String(error)}`);
          return { workspaces: [] };
        });
        for (const workspace of workspaces) {
          for (let attempt = 0; attempt < 2; attempt++) {
            const current = await scopeClient.workspacesList({}).then((result) =>
              result.workspaces.find((item) => item.id === workspace.id),
            ).catch((error) => {
              errors.push(`${scopeName}: read workspace ${workspace.id}: ${String(error)}`);
              return undefined;
            });
            if (!current) break;
            try {
              await scopeClient.workspacesDelete({ id: current.id, ifVersion: current.version });
              break;
            } catch (error) {
              if (attempt === 1) errors.push(`${scopeName}: delete workspace ${current.id}: ${String(error)}`);
            }
          }
        }
      };
      if (client) await cleanDashboard(client, "primary dashboard");
      if (secondDashboardClient) await cleanDashboard(secondDashboardClient, "second dashboard");
    } finally {
      await stopChild(secondWorker).catch((error) => errors.push(`stop second worker: ${String(error)}`));
      await stopChild(secondDashboardPtyFixtureWorker).catch((error) => errors.push(`stop second dashboard PTY fixture worker: ${String(error)}`));
      await stopKeeper(secondDashboardPtyFixtureDataDir).catch((error) => errors.push(`stop second dashboard PTY fixture keeper: ${String(error)}`));
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
          ROOST_COORDINATOR_DB: coordDbPath,
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

    const apiKeyPath = join(root, "api.key");
    const apiKey = await loadWorkerKey(apiKeyPath);
    seedTerminalDashboards(bunExecutable, coordDbPath, apiKey.fingerprint, apiKey.pubKey);
    const rawClient = await buildAuthorizedApiClient({
      coordinatorUrl: baseUrl,
      keyPath: apiKeyPath,
      label: "roost-terminal-test-api",
      skipTenantProbe: true,
    });
    client = withTerminalDashboard(rawClient, TERMINAL_TEST_DASHBOARD_ID);
    secondDashboardClient = withTerminalDashboard(rawClient, TERMINAL_TEST_SECOND_DASHBOARD_ID);
    const startWorker = createTerminalWorkerStarter(bunExecutable, baseUrl);
    const compilePtyFixture = createPtyFixtureCompiler(bunExecutable, ptyFixtureExecutable);

    const bootstrapToken = (await client.authMintBootstrap({ kind: "worker", label: WORKER_LABEL })).token;
    worker = startWorker({
      label: WORKER_LABEL,
      home,
      logPath: workerLogPath,
      dataDir: workerDataDir,
      tmpDir: childTmpDirs.worker,
      bootstrapToken,
    });
    const workerFp = await waitForTerminalWorkerRoutable(client, WORKER_LABEL, workerLogPath);

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
        const workerFp = await waitForTerminalWorkerRoutable(
          client!,
          SECOND_WORKER_LABEL,
          secondWorkerLogPath,
        );
        return { workerFp, label: SECOND_WORKER_LABEL, home: secondHome, logPath: secondWorkerLogPath };
      })();
      return secondWorkerStart;
    };
    const startPtyFixtureWorker = (): Promise<TerminalTestWorker> => {
      ptyFixtureWorkerStart ??= (async () => {
        compilePtyFixture();
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
        const workerFp = await waitForTerminalWorkerRoutable(
          client!,
          PTY_FIXTURE_WORKER_LABEL,
          ptyFixtureLogPath,
        );
        return {
          workerFp,
          label: PTY_FIXTURE_WORKER_LABEL,
          home: ptyFixtureHome,
          logPath: ptyFixtureLogPath,
        };
      })();
      return ptyFixtureWorkerStart;
    };
    const startSecondDashboardPtyFixtureWorker = (): Promise<TerminalTestWorker> => {
      secondDashboardPtyFixtureWorkerStart ??= (async () => {
        compilePtyFixture();
        const fixtureBootstrapToken = (
          await secondDashboardClient!.authMintBootstrap({
            kind: "worker",
            label: SECOND_DASHBOARD_PTY_FIXTURE_WORKER_LABEL,
          })
        ).token;
        secondDashboardPtyFixtureWorker = startWorker({
          label: SECOND_DASHBOARD_PTY_FIXTURE_WORKER_LABEL,
          home: secondDashboardPtyFixtureHome,
          logPath: secondDashboardPtyFixtureLogPath,
          dataDir: secondDashboardPtyFixtureDataDir,
          tmpDir: childTmpDirs.secondDashboardPtyFixtureWorker,
          bootstrapToken: fixtureBootstrapToken,
          shell: ptyFixtureExecutable,
        });
        const workerFp = await waitForTerminalWorkerRoutable(
          secondDashboardClient!,
          SECOND_DASHBOARD_PTY_FIXTURE_WORKER_LABEL,
          secondDashboardPtyFixtureLogPath,
        );
        return {
          workerFp,
          label: SECOND_DASHBOARD_PTY_FIXTURE_WORKER_LABEL,
          home: secondDashboardPtyFixtureHome,
          logPath: secondDashboardPtyFixtureLogPath,
        };
      })();
      return secondDashboardPtyFixtureWorkerStart;
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
      await waitForTerminalWorkerRoutable(client!, WORKER_LABEL, workerLogPath);
    };

    return {
      baseUrl,
      dashboardId: TERMINAL_TEST_DASHBOARD_ID,
      secondDashboardId: TERMINAL_TEST_SECOND_DASHBOARD_ID,
      workerFp,
      workerHome: home,
      coordLogPath,
      workerLogPath,
      secondWorkerLogPath,
      ptyFixtureWorkerLogPath: ptyFixtureLogPath,
      secondDashboardPtyFixtureWorkerLogPath: secondDashboardPtyFixtureLogPath,
      client,
      secondDashboardClient,
      startSecondWorker,
      startPtyFixtureWorker,
      startSecondDashboardPtyFixtureWorker,
      restartWorker,
      stop,
    };
  } catch (error) {
    const logs = `coord log:\n${logTail(coordLogPath)}\nworker log:\n${logTail(workerLogPath)}\nsecond worker log:\n${logTail(secondWorkerLogPath)}\nsecond dashboard PTY fixture worker log:\n${logTail(secondDashboardPtyFixtureLogPath)}`;
    await stop().catch(() => undefined);
    throw new Error(`${String(error)}\n${logs}`);
  }
}
