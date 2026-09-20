// Coordinator lifecycle for the terminal smoke stack: the first launch on an
// OS-assigned loopback port, and the stop/start pair a spec uses to take the
// coordinator away while the worker, its keeper and every PTY stay live.
// The stack owns everything else; this module owns only the coordinator child,
// so a stop here can never reach a worker or a keeper.

import type { RunningService } from "./stack-runtime.ts";
import {
  logTail,
  startCoordinatorService,
  stopChild,
  waitFor,
} from "./stack-runtime.ts";

const COORD_START_TIMEOUT_MS = 20_000;

export interface CoordinatorControlOptions {
  bunExecutable: string;
  /** Checkout the coordinator runs from; an upgrade run swaps it. */
  sourceRoot: string;
  root: string;
  home: string;
  tmpDir: string;
  dbPath: string;
  logPath: string;
  gitSha: string;
  /** Origins the coordinator must accept cross-origin RPC and Sync upgrades
   *  from. Only the 4104 default is pre-allowlisted in product code, so the
   *  harness's reserved local UI origins must be named here. */
  corsAllowedOrigins: readonly string[];
  /** Explicit peer settings used by direct-transport smoke stacks. */
  terminalPeerEnabled?: boolean;
  terminalPeerStunUrls?: readonly string[];
  /** Proof a relaunched coordinator serves RPC again. Called only on restart:
   *  the first boot has no authorized client yet and learns its port from the log. */
  probeReady: () => Promise<unknown>;
}

export interface CoordinatorControl {
  /** Origin the coordinator listens on; a stop/start pair preserves it. */
  readonly baseUrl: string;
  /** Stop only the coordinator child. Idempotent. */
  stop(): Promise<void>;
  /** Relaunch on the same port. Resolves once the coordinator accepts
   *  connections again; worker routability lags behind, so a caller that needs a
   *  routable worker must poll `workersList().routableFps`. */
  start(): Promise<void>;
}

export async function startCoordinatorControl(
  options: CoordinatorControlOptions,
): Promise<CoordinatorControl> {
  let child: RunningService | undefined = launch(options, "127.0.0.1:0");
  // The port is OS-assigned, so the log is the only place it exists; a restart
  // replays it verbatim, because browsers and clients already dialed it.
  const baseUrl = await waitFor("coordinator startup", COORD_START_TIMEOUT_MS, () => {
    const match = /"msg":"listening"[^\n]*"bind":"([^"]+)"/.exec(logTail(options.logPath));
    return match ? `http://${match[1]}` : undefined;
  }).catch(async (error) => {
    // A child that outlived its own failed boot would hold the port and the
    // coordinator database for every later run in the same job.
    await stopChild(child);
    child = undefined;
    throw new Error(`${error}\ncoord log:\n${logTail(options.logPath)}`);
  });
  const bind = baseUrl.slice("http://".length);
  return {
    baseUrl,
    stop: async () => {
      await stopChild(child);
      child = undefined;
    },
    start: async () => {
      if (child) return;
      child = launch(options, bind);
      await waitFor("coordinator startup", COORD_START_TIMEOUT_MS, () =>
        options.probeReady().then(() => true),
      ).catch((error) => {
        throw new Error(`${error}\ncoord log:\n${logTail(options.logPath)}`);
      });
    },
  };
}

function launch(options: CoordinatorControlOptions, bind: string): RunningService {
  return startCoordinatorService({
    bunExecutable: options.bunExecutable,
    sourceRoot: options.sourceRoot,
    root: options.root,
    home: options.home,
    tmpDir: options.tmpDir,
    bind,
    dbPath: options.dbPath,
    logPath: options.logPath,
    gitSha: options.gitSha,
    corsAllowedOrigins: options.corsAllowedOrigins,
    terminalPeerEnabled: options.terminalPeerEnabled,
    terminalPeerStunUrls: options.terminalPeerStunUrls,
  });
}
