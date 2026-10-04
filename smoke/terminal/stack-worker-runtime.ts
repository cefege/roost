// Terminal stack worker support owns child launch, routability waits, and fixture compilation.
// The stack lifecycle supplies isolated paths while this module keeps worker setup byte-identical.
// Every worker is `roost worker`; a `smoke`-featured build may also take the fault socket flags.

import { execFileSync, spawn } from "node:child_process";
import { openSync } from "node:fs";
import { join } from "node:path";
import type { AuthorizedApiClient } from "../support/coord-client.ts";
import {
  REPOSITORY_ROOT,
  childEnvironment,
  logTail,
  waitFor,
  type RunningService,
} from "./stack-runtime.ts";

import { ensureSmokeStackBinary } from "./stack-rust-binaries.ts";
const WORKER_READY_TIMEOUT_MS = 30_000;

export type TerminalWorkerPeerPortRange = {
  readonly min: number;
  readonly max: number;
};

export interface TerminalWorkerRuntime {
  /** The `roost` binary. It receives the ordinary `worker` subcommand. */
  workerExecutable: string;
  /** Arguments after `worker`: the fault socket flags a `smoke`-featured build accepts. */
  workerExecutableArgs?: readonly string[];
  /** The dx bundle the worker's local door serves. */
  webDist: string;
}

export interface TerminalWorkerStartConfig {
  label: string;
  home: string;
  logPath: string;
  dataDir: string;
  tmpDir: string;
  bootstrapToken: string;
  shell?: string;
  /** Loopback bind for the worker-served local UI; the harness assigns a
   *  distinct reserved port per worker, because the 4104 default collides
   *  between the workers of one stack and between concurrent stacks. */
  localUiBind?: string;
  /** Explicit peer runtime settings for hermetic direct-transport smoke cases. */
  terminalPeerEnabled?: boolean;
  terminalPeerBindAddress?: string;
  terminalPeerPortRange?: TerminalWorkerPeerPortRange;
  /** Build identity the worker reports. */
  gitSha?: string;
}

export function createTerminalWorkerStarter(
  coordinatorUrl: string,
  runtime: TerminalWorkerRuntime,
): (config: TerminalWorkerStartConfig) => RunningService {
  ensureSmokeStackBinary(runtime.workerExecutable, REPOSITORY_ROOT);
  const args = ["worker", ...(runtime.workerExecutableArgs ?? [])];
  return (config) => {
    const workerLog = openSync(config.logPath, "a");
    return {
      logPath: config.logPath,
      child: spawn(runtime.workerExecutable, args, {
        cwd: REPOSITORY_ROOT,
        env: childEnvironment(config.home, config.tmpDir, {
          ROOST_COORDINATOR_URL: coordinatorUrl,
          // Only the first boot redeems the token; persisted data owns the
          // identity on restart.
          ROOST_BOOTSTRAP_TOKEN: config.bootstrapToken,
          ROOST_WORKER_LABEL: config.label,
          ROOST_WORKER_DATA_DIR: config.dataDir,
          ROOST_WORKER_KEY_PATH: join(config.dataDir, "worker.key"),
          ROOST_KEEPER_QUIET: "1",
          // The local door serves the page from this key; whichever door serves
          // the page serves the same build the coordinator does.
          ROOST_WEB_DIST_PATH: runtime.webDist,
          ...(config.gitSha ? { GIT_SHA: config.gitSha, ROOST_GIT_SHA: config.gitSha } : {}),
          ...(config.localUiBind ? { ROOST_WORKER_LOCAL_UI_BIND: config.localUiBind } : {}),
          ...(config.terminalPeerEnabled === undefined
            ? {}
            : { ROOST_TERMINAL_PEER_ENABLED: config.terminalPeerEnabled ? "1" : "0" }),
          ...(config.terminalPeerBindAddress === undefined
            ? {}
            : { ROOST_TERMINAL_PEER_BIND_ADDRESS: config.terminalPeerBindAddress }),
          ...(config.terminalPeerPortRange === undefined
            ? {}
            : {
              ROOST_TERMINAL_PEER_PORT_RANGE:
                `${config.terminalPeerPortRange.min}-${config.terminalPeerPortRange.max}`,
            }),
          ...(config.shell ? { SHELL: config.shell, ROOST_SHELL: config.shell } : {}),
        }),
        stdio: ["ignore", workerLog, workerLog],
      }),
    };
  };
}

export function waitForTerminalWorkerRoutable(
  client: AuthorizedApiClient,
  label: string,
  logPath: string,
): Promise<string> {
  return waitFor(`${label} routable`, WORKER_READY_TIMEOUT_MS, async () => {
    const result = await client.workersList({});
    const candidate = result.workers.find((item) => item.label === label);
    return candidate && result.routableFps.includes(candidate.fp) ? candidate.fp : undefined;
  }).catch((error) => {
    throw new Error(`${error}\nworker log:\n${logTail(logPath)}`);
  });
}

export function createPtyFixtureCompiler(
  bunExecutable: string,
  fixtureExecutable: string,
): () => void {
  let compiled = false;
  return () => {
    if (compiled) return;
    execFileSync(
      bunExecutable,
      [
        "build",
        "--compile",
        join(REPOSITORY_ROOT, "smoke", "terminal", "pty-fixture.ts"),
        "--outfile",
        fixtureExecutable,
      ],
      { cwd: REPOSITORY_ROOT, stdio: "pipe" },
    );
    compiled = true;
  };
}
