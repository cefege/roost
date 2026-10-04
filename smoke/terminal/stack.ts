// The terminal smoke stack starts an isolated coordinator, workers, keepers, and API key.
// Playwright fixtures call this lifecycle and receive lazy worker factories plus cleanup.
// Every child gets isolated state and temp roots while the returned stop closes all resources.

import { mkdirSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createCoordClient, type AuthorizedApiClient } from "../support/coord-client.ts";
import { loadWorkerKey, mintJwt } from "../support/worker-key.ts";
import {
  authorizeTerminalTestApiKey,
  cleanInstallResources,
  logTail,
  stopChild,
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
import { peerFaultControlsUnavailable, resolveSmokeStackExecutables, smokeStackDescription } from "./stack-executables.ts";
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
  TerminalTestStack,
  TerminalTestStackOptions,
  TerminalTestWorker,
} from "./stack-types.ts";
export type {
  TerminalPeerFaults,
  TerminalPeerSmokeOptions,
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
/** Build identity every child reports; nothing in this suite compares it. */
const STACK_GIT_SHA = "dev";
export async function startTerminalTestStack(
  options: TerminalTestStackOptions = {},
): Promise<TerminalTestStack> {
  // Resolved before anything is created or spawned, so a missing pin fails
  // fast; printed because a spec failure is only attributable when the run
  // says which binaries and which page it drove.
  const executables = resolveSmokeStackExecutables();
  console.log(smokeStackDescription(executables));
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
  const coordDbPath = join(root, "coord.db");
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
  const bunExecutable = process.env.ROOST_TEST_BUN ?? "bun";
  const terminalPeer = options.terminalPeer;
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
      if (client) await cleanInstallResources(client, errors);
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
      await stopKeeper(workerDataDir).catch((error) => errors.push(`stop keeper: ${String(error)}`));
      await localUi.closeAll().catch((error) => errors.push(`release local UI ports: ${String(error)}`));
      await (coordinator?.stop() ?? Promise.resolve()).catch((error) => errors.push(`stop coordinator: ${String(error)}`));
      try { rmSync(root, { recursive: true, force: true }); } catch (error) { errors.push(`remove test root: ${String(error)}`); }
    }
    if (errors.length > 0) throw new Error(`terminal stack cleanup failed:\n${errors.join("\n")}`);
  };

  let workerRuntime: TerminalWorkerRuntime = {
    workerExecutable: executables.workerExecutable,
    webDist: executables.webDist,
  };

  try {
    if (terminalPeer?.enableFaults) {
      // The reason is resolved BEFORE anything is spawned so a spec that
      // cannot run this tier says so as a skip, with every other stack log
      // still attached, instead of aborting mid-start and leaving a failure
      // whose only evidence is that refusal.
      const faultsUnavailable = peerFaultControlsUnavailable(executables.workerExecutable);
      if (faultsUnavailable !== null) throw new Error(faultsUnavailable);
      peerFaultControl = await startStackPeerFaultControl(root);
      directInputHold = await startDirectInputHold(root);
      // A `smoke`-featured `roost worker` connects to both sockets at boot.
      workerRuntime = {
        ...workerRuntime,
        workerExecutableArgs: [
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
      coordExecutable: executables.coordExecutable,
      webDist: executables.webDist,
      root,
      home,
      tmpDir: childTmpDirs.coord,
      dbPath: coordDbPath,
      logPath: coordLogPath,
      gitSha: STACK_GIT_SHA,
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
    client = createCoordClient({ baseUrl, getJwt: () => mintJwt(apiKey, "roost-coordinator") });
    const startWorker = createTerminalWorkerStarter(baseUrl, workerRuntime);
    const compilePtyFixture = createPtyFixtureCompiler(bunExecutable, ptyFixtureExecutable);
    const fixtureLaunch = {
      coordinatorUrl: baseUrl,
      runtime: workerRuntime,
      compileFixture: compilePtyFixture,
      fixtureExecutable: ptyFixtureExecutable,
      client: client!,
      terminalPeerEnabled: terminalPeer?.workerEnabled ?? false,
      terminalPeerBindAddress: terminalPeer?.workerBindAddress,
      terminalPeerPortRange: terminalPeer?.workerPortRange,
    };

    const bootstrapToken = (await client.authMintBootstrap({ kind: "worker", label: WORKER_LABEL })).token;
    const primaryWorkerConfig: TerminalWorkerStartConfig = {
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
      gitSha: STACK_GIT_SHA,
    };
    await workerLocalUi.release();
    worker = startWorker(primaryWorkerConfig);
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
          gitSha: STACK_GIT_SHA,
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
      worker = startWorker(primaryWorkerConfig);
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
      stop,
    };
  } catch (error) {
    const logs = `coord log:\n${logTail(coordLogPath)}\nworker log:\n${logTail(workerLogPath)}\nsecond worker log:\n${logTail(secondWorkerLogPath)}\nPTY fixture worker log:\n${logTail(ptyFixtureLogPath)}\nsecond PTY fixture worker log:\n${logTail(secondPtyFixtureLogPath)}`;
    await stop().catch(() => undefined);
    throw new Error(`${String(error)}\n${logs}`);
  }
}

