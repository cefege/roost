// The terminal smoke stack starts an isolated coordinator, workers, keepers, and API key.
// Playwright fixtures call this lifecycle and receive lazy worker factories plus cleanup.
// Every child gets isolated state and temp roots while the returned stop closes all resources.
// Split coordinator and worker release checkouts let the upgrade tier run this
// stack as an existing install a prior release created and a new one takes over.

import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildAuthorizedApiClient, type AuthorizedApiClient } from "../../apps/roost-cli/src/api.ts";
import { loadWorkerKey } from "../../apps/worker/src/jwt.ts";
import {
  REPOSITORY_ROOT,
  authorizeTerminalTestApiKey,
  logTail,
  startCoordinatorService,
  stopChild,
  stopDeployedWorker,
  stopKeeper,
  waitFor,
  type RunningService,
} from "./stack-runtime.ts";
import {
  createPtyFixtureCompiler,
  createTerminalWorkerStarter,
  waitForTerminalWorkerRoutable,
  type TerminalWorkerStartConfig,
} from "./stack-worker-runtime.ts";
const WORKER_LABEL = "roost-terminal-test";
const SECOND_WORKER_LABEL = "roost-terminal-test-second";
const PTY_FIXTURE_WORKER_LABEL = "roost-terminal-test-pty-fixture";
const COORD_START_TIMEOUT_MS = 20_000;

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
  /** Coordinator database the CLI reads for deploy admission. */
  coordDbPath: string;
  /** Key the harness authorized, so a deploy can call the same coordinator. */
  apiKeyPath: string;
  /** Persisted primary-worker launch spec, for a deploy running out of process. */
  workerServiceSpecPath: string;
  /** Process id of the running primary worker. */
  workerPid(): number | undefined;
  /** Take teardown ownership of a worker a deploy left running. */
  adoptDeployedWorker(pid: number): void;
  /** Release the primary worker runs, so a deploy can name what it replaces. */
  workerRelease: TerminalReleaseCheckout;
  stop(): Promise<void>;
};

export type TerminalReleaseCheckout = {
  /** Checkout the process runs from. */
  sourceRoot: string;
  /** Build identity it reports; deploy admission and convergence compare it. */
  gitSha: string;
};

export type TerminalTestStackOptions = {
  // Keep the caller's real HOME instead of the isolated temp one. Needed only
  // by the agent smoke: the worker forks `omp`, which reads its model
  // credentials from the real ~/.omp — under a temp HOME every turn fails
  // unauthenticated. Coord/worker state stays isolated either way (their paths
  // are ROOST_* env overrides, not HOME-derived).
  useRealHome?: boolean;
  // Releases the coordinator and the workers run from. Both default to this
  // checkout; the upgrade tier points them at different ones, because a real
  // upgrade moves the coordinator first and the workers afterwards.
  coordRelease?: Partial<TerminalReleaseCheckout>;
  workerRelease?: Partial<TerminalReleaseCheckout>;
  // Coordinator database to boot over. Default is a fresh one under the test
  // root; an upgrade run supplies one a prior release already migrated.
  coordDbPath?: string;
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
  const coordDbPath = options.coordDbPath ?? join(root, "coord.db");
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
  const workerServiceSpecPath = join(root, "worker-service.json");
  const bunExecutable = process.env.ROOST_TEST_BUN ?? "bun";
  const coordRelease: TerminalReleaseCheckout = {
    sourceRoot: options.coordRelease?.sourceRoot ?? REPOSITORY_ROOT,
    gitSha: options.coordRelease?.gitSha ?? "dev",
  };
  const workerRelease: TerminalReleaseCheckout = {
    sourceRoot: options.workerRelease?.sourceRoot ?? coordRelease.sourceRoot,
    gitSha: options.workerRelease?.gitSha ?? coordRelease.gitSha,
  };
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
  let deployedWorkerPid: number | undefined;
  let secondWorker: RunningService | undefined;
  let secondWorkerStart: Promise<TerminalTestWorker> | undefined;
  let ptyFixtureWorker: RunningService | undefined;
  let ptyFixtureWorkerStart: Promise<TerminalTestWorker> | undefined;
  let client: AuthorizedApiClient | undefined;

  const stop = async () => {
    const errors: string[] = [];
    try {
      const cleanInstallResources = async (installClient: AuthorizedApiClient): Promise<void> => {
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
      };
      if (client) await cleanInstallResources(client);
    } finally {
      await stopChild(secondWorker).catch((error) => errors.push(`stop second worker: ${String(error)}`));
      await stopChild(ptyFixtureWorker).catch((error) => errors.push(`stop PTY fixture worker: ${String(error)}`));
      await stopKeeper(ptyFixtureDataDir).catch((error) => errors.push(`stop PTY fixture keeper: ${String(error)}`));
      await stopKeeper(secondWorkerDataDir).catch((error) => errors.push(`stop second keeper: ${String(error)}`));
      await stopChild(worker).catch((error) => errors.push(`stop worker: ${String(error)}`));
      await stopDeployedWorker(deployedWorkerPid).catch((error) => errors.push(`stop deployed worker: ${String(error)}`));
      await stopKeeper(workerDataDir).catch((error) => errors.push(`stop keeper: ${String(error)}`));
      await stopChild(coord).catch((error) => errors.push(`stop coordinator: ${String(error)}`));
      try { rmSync(root, { recursive: true, force: true }); } catch (error) { errors.push(`remove test root: ${String(error)}`); }
    }
    if (errors.length > 0) throw new Error(`terminal stack cleanup failed:\n${errors.join("\n")}`);
  };

  try {
    coord = startCoordinatorService({
      bunExecutable,
      sourceRoot: coordRelease.sourceRoot,
      root,
      home,
      tmpDir: childTmpDirs.coord,
      bind: "127.0.0.1:0",
      dbPath: coordDbPath,
      logPath: coordLogPath,
      gitSha: coordRelease.gitSha,
    });
    const baseUrl = await waitFor("coordinator startup", COORD_START_TIMEOUT_MS, () => {
      const match = /"msg":"listening"[^\n]*"bind":"([^"]+)"/.exec(logTail(coordLogPath));
      return match ? `http://${match[1]}` : undefined;
    }).catch((error) => { throw new Error(`${error}\ncoord log:\n${logTail(coordLogPath)}`); });

    const apiKeyPath = join(root, "api.key");
    const apiKey = await loadWorkerKey(apiKeyPath);
    authorizeTerminalTestApiKey(bunExecutable, coordDbPath, apiKey.fingerprint, apiKey.pubKey);
    client = await buildAuthorizedApiClient({
      coordinatorUrl: baseUrl,
      keyPath: apiKeyPath,
      label: "roost-terminal-test-api",
    });
    const startWorker = createTerminalWorkerStarter(bunExecutable, baseUrl, workerRelease.sourceRoot);
    const compilePtyFixture = createPtyFixtureCompiler(bunExecutable, ptyFixtureExecutable);

    const bootstrapToken = (await client.authMintBootstrap({ kind: "worker", label: WORKER_LABEL })).token;
    const workerServiceSpec: TerminalWorkerStartConfig = {
      label: WORKER_LABEL,
      home,
      logPath: workerLogPath,
      dataDir: workerDataDir,
      tmpDir: childTmpDirs.worker,
      bootstrapToken,
      gitSha: workerRelease.gitSha,
    };
    // A deploy runs out of process and must relaunch this exact identity, so the
    // launch spec is persisted instead of restated at the second call site.
    writeFileSync(workerServiceSpecPath, `${JSON.stringify(workerServiceSpec, null, 2)}\n`, { mode: 0o600 });
    worker = startWorker(workerServiceSpec);
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

    // Full primary-worker bounce, keeping coord and the persisted identity.
    // The keeper is deliberately left alone: it is designed to outlive the
    // worker, and agent sessions never touch it anyway.
    const restartWorker = async () => {
      await stopChild(worker);
      worker = startWorker(workerServiceSpec);
      await waitForTerminalWorkerRoutable(client!, WORKER_LABEL, workerLogPath);
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
      coordDbPath,
      apiKeyPath,
      workerServiceSpecPath,
      workerPid: () => worker?.child.pid,
      adoptDeployedWorker: (pid) => { deployedWorkerPid = pid; },
      workerRelease,
      stop,
    };
  } catch (error) {
    const logs = `coord log:\n${logTail(coordLogPath)}\nworker log:\n${logTail(workerLogPath)}\nsecond worker log:\n${logTail(secondWorkerLogPath)}`;
    await stop().catch(() => undefined);
    throw new Error(`${String(error)}\n${logs}`);
  }
}
