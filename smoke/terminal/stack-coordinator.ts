// Coordinator lifecycle for the terminal smoke stack: its first launch binds an
// OS-assigned loopback port by default or a reserved local-first port, and the
// stop/start pair lets a spec take the coordinator away while workers and PTYs stay live.
// The stack owns everything else; this module owns only the coordinator child,
// so a stop here can never reach a worker or a keeper.

import type { RunningService } from "./stack-runtime.ts";
import {
  logTail,
  startCoordinatorService,
  stopChild,
  waitFor,
} from "./stack-runtime.ts";
import { coordinatorRuntimeOverrides, resolveSmokeStackExecutables } from "./stack-executables.ts";
import { ensureSmokeStackBinary } from "./stack-rust-binaries.ts";

const COORD_START_TIMEOUT_MS = 20_000;

/**
 * How the coordinator child is launched. A packaged binary replaces the
 * TypeScript entrypoint; an empty runtime is the TypeScript launch.
 */
export interface TerminalCoordinatorRuntime {
  /** Exact compiled `roost` binary. It receives the ordinary `coord` subcommand. */
  coordExecutable?: string;
}

export interface CoordinatorControlOptions {
  bunExecutable: string;
  /** Exact compiled `roost` binary; absent resolves ROOST_SMOKE_COORD_EXECUTABLE. */
  coordExecutable?: string;
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
  /** Fixed local bind for a browser-origin smoke flow; omitted keeps port zero. */
  initialBind?: string;
  /** Explicit CSP profile; omitted preserves ordinary terminal-smoke behavior. */
  relaxedCsp?: boolean;
  /** Explicit endpoint values; empty values clear public URLs for local-only smoke. */
  webPublicUrl?: string;
  coordinatorPublicUrl?: string;
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
  const runtime: TerminalCoordinatorRuntime = options.coordExecutable
    ? { coordExecutable: options.coordExecutable }
    : coordinatorRuntimeOverrides(resolveSmokeStackExecutables());
  // A stale binary would make this run prove an older build than the tree it
  // claims to test, so the build belongs here rather than in a
  // remember-to-build-first note nobody reads.
  ensureSmokeStackBinary(runtime.coordExecutable, options.sourceRoot);
  let child: RunningService | undefined = launch(options, runtime, options.initialBind ?? "127.0.0.1:0");
  // The first launch may use a reserved port; startup logging supplies the resolved
  // bind, which restarts replay verbatim because browsers and clients already dialed it.
  const baseUrl = await waitFor("coordinator startup", COORD_START_TIMEOUT_MS, () => {
    const bound = listeningBind(logTail(options.logPath));
    return bound ? `http://${bound}` : undefined;
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
      child = launch(options, runtime, bind);
      await waitFor("coordinator startup", COORD_START_TIMEOUT_MS, () =>
        options.probeReady().then(() => true),
      ).catch((error) => {
        throw new Error(`${error}\ncoord log:\n${logTail(options.logPath)}`);
      });
    },
  };
}

/**
 * The bind a coordinator reported while starting, or undefined.
 *
 * Read as JSON rather than matched as text because the two implementations
 * differ in both key order and message key: the TypeScript line leads with
 * `msg`, while the Rust formatter sorts the caller's `bind` ahead of the rest
 * and carries the message under `message`. A regex written for one shape
 * silently never matches the other, which reads as a coordinator that never
 * boots even though it is listening.
 */
export function listeningBind(logText: string): string | undefined {
  for (const line of logText.split("\n")) {
    let event: unknown;
    try {
      event = JSON.parse(line);
    } catch {
      continue;
    }
    if (typeof event !== "object" || event === null) continue;
    const { msg, message, bind } = event as { msg?: unknown; message?: unknown; bind?: unknown };
    const text = typeof msg === "string" ? msg : message;
    if (typeof text !== "string" || !text.includes("listening")) continue;
    if (typeof bind === "string") return bind;
  }
  return undefined;
}

function launch(
  options: CoordinatorControlOptions,
  runtime: TerminalCoordinatorRuntime,
  bind: string,
): RunningService {
  return startCoordinatorService({
    bunExecutable: options.bunExecutable,
    coordExecutable: runtime.coordExecutable,
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
    relaxedCsp: options.relaxedCsp,
    webPublicUrl: options.webPublicUrl,
    coordinatorPublicUrl: options.coordinatorPublicUrl,
  });
}
