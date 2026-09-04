// Terminal stack worker support owns child launch, routability waits, and fixture compilation.
// The stack lifecycle supplies isolated paths while this module keeps worker setup byte-identical.
// Sharing one compiler closure preserves lazy compilation across both fixture-worker factories.

import { execFileSync, spawn } from "node:child_process";
import { openSync } from "node:fs";
import { join } from "node:path";
import type { AuthorizedApiClient } from "../../apps/roost-cli/src/api.ts";
import {
  REPOSITORY_ROOT,
  childEnvironment,
  logTail,
  waitFor,
  type RunningService,
} from "./stack-runtime.ts";

const WORKER_READY_TIMEOUT_MS = 30_000;

export interface TerminalWorkerStartConfig {
  label: string;
  home: string;
  logPath: string;
  dataDir: string;
  tmpDir: string;
  bootstrapToken: string;
  shell?: string;
}

export function createTerminalWorkerStarter(
  bunExecutable: string,
  coordinatorUrl: string,
): (config: TerminalWorkerStartConfig) => RunningService {
  return (config) => {
    const workerLog = openSync(config.logPath, "a");
    return {
      logPath: config.logPath,
      child: spawn(bunExecutable, ["apps/worker/src/main.ts"], {
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
