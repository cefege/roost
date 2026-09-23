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
  stopChild,
  stopDeployedWorker,
  stopKeeper,
  type RunningService,
} from "./stack-runtime.ts";
import {
  createPtyFixtureCompiler,
  createTerminalWorkerStarter,
  waitForTerminalWorkerRoutable,
  type TerminalWorkerRuntime,
  type TerminalWorkerStartConfig,
} from "./stack-worker-runtime.ts";
import type { DelayedWorkerLink } from "./delayed-worker-link.ts";
import { createFixtureWorkerStarter, type PtyFixtureWorkerStartOptions } from "./stack-fixture-worker.ts";
import { createLocalUiOrigins } from "./stack-local-ui.ts";
import { startCoordinatorControl, type CoordinatorControl } from "./stack-coordinator.ts";
import {
  startDirectInputHold,
  type DirectInputHold,
} from "./stack-direct-input-hold.ts";
import {
  startStackPeerFaultControl,
  type StackPeerFaultControl,
} from "./stack-peer-fault-control.ts";
import { createTerminalPeerFaults } from "./stack-peer-faults.ts";
import type {
  TerminalReleaseCheckout,
  TerminalTestStack,
  TerminalTestStackOptions,
  TerminalTestWorker,
} from "./stack-types.ts";
export type {
  TerminalPeerFaults,
  TerminalPeerSmokeOptions,
  TerminalReleaseCheckout,
  TerminalTestStack,
  TerminalTestStackOptions,
  TerminalTestWorker,
} from "./stack-types.ts";
export type { PtyFixtureWorkerStartOptions } from "./stack-fixture-worker.ts";
export type {
  PeerFaultMalformedPacketKind,
  PeerFaultOfferKind,
} from "./stack-peer-fault-control.ts";
const WORKER_LABEL = "roost-terminal-test";
const SECOND_WORKER_LABEL = "roost-terminal-test-second";
const PTY_FIXTURE_WORKER_LABEL = "roost-terminal-test-pty-fixture";
const SECOND_PTY_FIXTURE_WORKER_LABEL = "roost-terminal-test-pty-fixture-second";
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
  const home = options.useRealHome === true
    ? (process.env.HOME ?? join(root, "home"))
    : join(root, "home");
  const secondHome = join(root, "second-home");
  const coordLogPath = join(root, "coord.log");
  const coordDbPath = options.coordDbPath ?? join(root, "coord.db");
  const workerLogPath = join(root, "worker.log");
  const secondWorkerLogPath = join(root, "second-worker.log");
  const ptyFixtureHome = join(root, "pty-fixture-home");
  const ptyFixtureLogPath = join(root, "pty-fixture-worker.log");
  const ptyFixtureDataDir = join(root, "pty-fixture-worker-data");
  const secondPtyFixtureHome = join(root, "pty-fixture-second-home");
  const secondPtyFixtureLogPath = join(root, "pty-fixture-second-worker.log");
  const secondPtyFixtureDataDir = join(root, "pty-fixture-second-worker-data");
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
  const terminalPeer = options.terminalPeer;
  let workerRuntime: TerminalWorkerRuntime = options.workerExecutable
    ? { workerExecutable: options.workerExecutable }
    : {};
  mkdirSync(home, { recursive: true });
  mkdirSync(secondHome, { recursive: true });
  mkdirSync(ptyFixtureHome, { recursive: true });
  mkdirSync(secondPtyFixtureHome, { recursive: true });
  const childTmpDirs = {
    coord: join(root, "coord-tmp"),
    worker: join(root, "worker-tmp"),
    secondWorker: join(root, "second-worker-tmp"),
    ptyFixtureWorker: join(root, "pty-fixture-worker-tmp"),
    secondPtyFixtureWorker: join(root, "pty-fixture-second-worker-tmp"),
  };
  for (const dir of Object.values(childTmpDirs)) mkdirSync(dir, { recursive: true });
  let coordinator: CoordinatorControl | undefined;
  let worker: RunningService | undefined;
  let deployedWorkerPid: number | undefined;
  let secondWorker: RunningService | undefined;
  let secondWorkerStart: Promise<TerminalTestWorker> | undefined;
  let ptyFixtureWorker: RunningService | undefined;
  let ptyFixtureWorkerLink: DelayedWorkerLink | undefined;
  let secondPtyFixtureWorker: RunningService | undefined;
  let secondPtyFixtureWorkerLink: DelayedWorkerLink | undefined;
  let client: AuthorizedApiClient | undefined;
  let directInputHold: DirectInputHold | undefined;
  let peerFaultControl: StackPeerFaultControl | undefined;
  const localUi = createLocalUiOrigins();

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
      await directInputHold?.stop().catch((error) => errors.push(`stop direct input hold: ${String(error)}`));
      await peerFaultControl?.stop().catch((error) => errors.push(`stop terminal peer fault control: ${String(error)}`));
      await stopChild(secondWorker).catch((error) => errors.push(`stop second worker: ${String(error)}`));
      await stopChild(secondPtyFixtureWorker).catch((error) => {
        errors.push(`stop second PTY fixture worker: ${String(error)}`);
      });
      await secondPtyFixtureWorkerLink?.stop().catch((error) => {
        errors.push(`stop second PTY fixture worker link: ${String(error)}`);
      });
      await stopKeeper(secondPtyFixtureDataDir).catch((error) => {
        errors.push(`stop second PTY fixture keeper: ${String(error)}`);
      });
      await stopChild(ptyFixtureWorker).catch((error) => errors.push(`stop PTY fixture worker: ${String(error)}`));
      await ptyFixtureWorkerLink?.stop().catch((error) => {
        errors.push(`stop PTY fixture worker link: ${String(error)}`);
      });
      await stopKeeper(ptyFixtureDataDir).catch((error) => errors.push(`stop PTY fixture keeper: ${String(error)}`));
      await stopKeeper(secondWorkerDataDir).catch((error) => errors.push(`stop second keeper: ${String(error)}`));
      await stopChild(worker).catch((error) => errors.push(`stop worker: ${String(error)}`));
      await stopDeployedWorker(deployedWorkerPid).catch((error) => errors.push(`stop deployed worker: ${String(error)}`));
      await stopKeeper(workerDataDir).catch((error) => errors.push(`stop keeper: ${String(error)}`));
      await localUi.closeAll().catch((error) => errors.push(`release local UI ports: ${String(error)}`));
      await (coordinator?.stop() ?? Promise.resolve()).catch((error) => errors.push(`stop coordinator: ${String(error)}`));
      try { rmSync(root, { recursive: true, force: true }); } catch (error) { errors.push(`remove test root: ${String(error)}`); }
    }
    if (errors.length > 0) throw new Error(`terminal stack cleanup failed:\n${errors.join("\n")}`);
  };

  try {
    if (terminalPeer?.enableFaults) {
      if (options.workerExecutable) {
        throw new Error("terminal peer fault controls require a source worker");
      }
      if (process.platform === "win32") {
        throw new Error("terminal peer fault controls are unavailable on Windows");
      }
      peerFaultControl = await startStackPeerFaultControl(root);
      directInputHold = await startDirectInputHold(root);
      workerRuntime = {
        sourceEntrypoint: join(REPOSITORY_ROOT, "smoke", "terminal", "stack-direct-input-worker.ts"),
        sourceEntrypointArgs: [
          `--direct-input-hold-socket=${directInputHold.socketPath}`,
          `--terminal-peer-fault-socket=${peerFaultControl.socketPath}`,
        ],
      };
    }
    // Every local UI port is reserved before the coordinator launches: product
    // code pre-allowlists only the 4104 default, so the coordinator has to be
    // told these origins at boot and on every relaunch.
    const workerLocalUi = await localUi.reserve(WORKER_LABEL);
    const secondWorkerLocalUi = await localUi.reserve(SECOND_WORKER_LABEL);
    const ptyFixtureLocalUi = await localUi.reserve(PTY_FIXTURE_WORKER_LABEL);
    const secondPtyFixtureLocalUi = await localUi.reserve(SECOND_PTY_FIXTURE_WORKER_LABEL);
    const coordinatorLocalUi = options.localFirst
      ? await localUi.reserve("roost-coordinator")
      : undefined;
    await coordinatorLocalUi?.release();
    coordinator = await startCoordinatorControl({
      bunExecutable,
      sourceRoot: coordRelease.sourceRoot,
      root,
      home,
      tmpDir: childTmpDirs.coord,
      dbPath: coordDbPath,
      logPath: coordLogPath,
      gitSha: coordRelease.gitSha,
      initialBind: coordinatorLocalUi?.bind,
      relaxedCsp: options.localFirst ? false : undefined,
      webPublicUrl: options.localFirst ? "" : undefined,
      coordinatorPublicUrl: options.localFirst ? "" : undefined,
      corsAllowedOrigins: localUi.origins(),
      terminalPeerEnabled: terminalPeer?.coordinatorEnabled ?? false,
      terminalPeerStunUrls: terminalPeer?.coordinatorStunUrls,
      probeReady: () => client!.workersList({}),
    });
    const baseUrl = coordinator.baseUrl;

    const apiKeyPath = join(root, "api.key");
    const apiKey = await loadWorkerKey(apiKeyPath);
    authorizeTerminalTestApiKey(bunExecutable, coordDbPath, apiKey.fingerprint, apiKey.pubKey);
    client = await buildAuthorizedApiClient({
      coordinatorUrl: baseUrl,
      keyPath: apiKeyPath,
      label: "roost-terminal-test-api",
    });
    const startWorker = createTerminalWorkerStarter(
      bunExecutable,
      baseUrl,
      workerRelease.sourceRoot,
      workerRuntime,
    );
    const compilePtyFixture = createPtyFixtureCompiler(bunExecutable, ptyFixtureExecutable);
    const fixtureLaunch = {
      bunExecutable,
      coordinatorUrl: baseUrl,
      runtime: workerRuntime,
      compileFixture: compilePtyFixture,
      sourceRoot: workerRelease.sourceRoot,
      fixtureExecutable: ptyFixtureExecutable,
      client: client!,
      terminalPeerEnabled: terminalPeer?.workerEnabled ?? false,
      terminalPeerBindAddress: terminalPeer?.workerBindAddress,
      terminalPeerPortRange: terminalPeer?.workerPortRange,
    };

    const bootstrapToken = (await client.authMintBootstrap({ kind: "worker", label: WORKER_LABEL })).token;
    const workerServiceSpec: TerminalWorkerStartConfig = {
      label: WORKER_LABEL,
      home,
      logPath: workerLogPath,
      dataDir: workerDataDir,
      tmpDir: childTmpDirs.worker,
      bootstrapToken,
      localUiBind: workerLocalUi.bind,
      terminalPeerEnabled: terminalPeer?.workerEnabled,
      terminalPeerBindAddress: terminalPeer?.workerBindAddress,
      terminalPeerPortRange: terminalPeer?.workerPortRange,
      gitSha: workerRelease.gitSha,
    };
    // A deploy runs out of process and must relaunch this exact identity, so the
    // launch spec is persisted instead of restated at the second call site.
    writeFileSync(workerServiceSpecPath, `${JSON.stringify(workerServiceSpec, null, 2)}\n`, { mode: 0o600 });
    await workerLocalUi.release();
    worker = startWorker(workerServiceSpec);
    const workerFp = await waitForTerminalWorkerRoutable(client, WORKER_LABEL, workerLogPath);
    workerLocalUi.record(workerFp);

    const startSecondWorker = (): Promise<TerminalTestWorker> => {
      secondWorkerStart ??= (async () => {
        const secondBootstrapToken = (
          await client!.authMintBootstrap({ kind: "worker", label: SECOND_WORKER_LABEL })
        ).token;
        await secondWorkerLocalUi.release();
        secondWorker = startWorker({
          label: SECOND_WORKER_LABEL,
          home: secondHome,
          logPath: secondWorkerLogPath,
          dataDir: secondWorkerDataDir,
          tmpDir: childTmpDirs.secondWorker,
          bootstrapToken: secondBootstrapToken,
          localUiBind: secondWorkerLocalUi.bind,
          terminalPeerEnabled: terminalPeer?.workerEnabled,
          terminalPeerBindAddress: terminalPeer?.workerBindAddress,
          terminalPeerPortRange: terminalPeer?.workerPortRange,
          gitSha: workerRelease.gitSha,
        });
        const workerFp = await waitForTerminalWorkerRoutable(
          client!,
          SECOND_WORKER_LABEL,
          secondWorkerLogPath,
        );
        secondWorkerLocalUi.record(workerFp);
        return { workerFp, label: SECOND_WORKER_LABEL, home: secondHome, logPath: secondWorkerLogPath };
      })();
      return secondWorkerStart;
    };
    const startPtyFixtureWorker = createFixtureWorkerStarter({
      ...fixtureLaunch,
      localUi: ptyFixtureLocalUi,
      paths: {
        label: PTY_FIXTURE_WORKER_LABEL,
        home: ptyFixtureHome,
        logPath: ptyFixtureLogPath,
        dataDir: ptyFixtureDataDir,
        tmpDir: childTmpDirs.ptyFixtureWorker,
      },
      onWorkerStarted: (service) => { ptyFixtureWorker = service; },
      onLinkStarted: (link) => { ptyFixtureWorkerLink = link; },
    });
    const startSecondPtyFixtureWorker = createFixtureWorkerStarter({
      ...fixtureLaunch,
      localUi: secondPtyFixtureLocalUi,
      paths: {
        label: SECOND_PTY_FIXTURE_WORKER_LABEL,
        home: secondPtyFixtureHome,
        logPath: secondPtyFixtureLogPath,
        dataDir: secondPtyFixtureDataDir,
        tmpDir: childTmpDirs.secondPtyFixtureWorker,
      },
      onWorkerStarted: (service) => { secondPtyFixtureWorker = service; },
      onLinkStarted: (link) => { secondPtyFixtureWorkerLink = link; },
    });

    // Full primary-worker bounce, keeping coord and the persisted identity.
    // The keeper is deliberately left alone: it is designed to outlive the
    // worker, and agent sessions never touch it anyway.
    const restartWorker = async () => {
      await stopChild(worker);
      worker = startWorker(workerServiceSpec);
      await waitForTerminalWorkerRoutable(client!, WORKER_LABEL, workerLogPath);
    };
    const { stop: stopCoordinator, start: startCoordinator } = coordinator;
    const peerFaults = directInputHold && peerFaultControl
      ? createTerminalPeerFaults(directInputHold, peerFaultControl)
      : null;

    return {
      baseUrl,
      workerFp,
      workerHome: home,
      coordLogPath,
      workerLogPath,
      secondWorkerLogPath,
      ptyFixtureWorkerLogPath: ptyFixtureLogPath,
      secondPtyFixtureWorkerLogPath: secondPtyFixtureLogPath,
      disableLoopbackProbe: terminalPeer?.disableLoopbackProbe === true,
      peerFaults,
      client,
      startSecondWorker,
      startPtyFixtureWorker,
      startSecondPtyFixtureWorker,
      restartWorker,
      get ptyFixtureWorkerLink() { return ptyFixtureWorkerLink ?? null; },
      stopCoordinator,
      startCoordinator,
      localUiUrl: localUi.url,
      coordDbPath,
      apiKeyPath,
      workerServiceSpecPath,
      workerPid: () => worker?.child.pid,
      adoptDeployedWorker: (pid) => { deployedWorkerPid = pid; },
      workerRelease,
      stop,
    };
  } catch (error) {
    const logs = `coord log:\n${logTail(coordLogPath)}\nworker log:\n${logTail(workerLogPath)}\nsecond worker log:\n${logTail(secondWorkerLogPath)}\nPTY fixture worker log:\n${logTail(ptyFixtureLogPath)}\nsecond PTY fixture worker log:\n${logTail(secondPtyFixtureLogPath)}`;
    await stop().catch(() => undefined);
    throw new Error(`${String(error)}\n${logs}`);
  }
}

