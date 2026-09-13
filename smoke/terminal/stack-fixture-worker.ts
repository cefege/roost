// Fixture-worker launch support owns the optional delayed coordinator link and fixture shell identity.
// The terminal stack supplies isolated paths and captures each started child for teardown.
// It reuses the ordinary worker starter so fixture workers retain the real auth and keeper lifecycle.

import type { AuthorizedApiClient } from "../../apps/roost-cli/src/api.ts";
import { startDelayedWorkerLink, type DelayedWorkerLink } from "./delayed-worker-link.ts";
import {
  createTerminalWorkerStarter,
  waitForTerminalWorkerRoutable,
} from "./stack-worker-runtime.ts";
import type { RunningService } from "./stack-runtime.ts";

export interface FixtureWorkerPaths {
  label: string;
  home: string;
  logPath: string;
  dataDir: string;
  tmpDir: string;
}

export interface FixtureWorkerIdentity {
  workerFp: string;
  label: string;
  home: string;
  logPath: string;
}

export type PtyFixtureWorkerStartOptions = {
  /** Inject only the requested worker-link delay; browsers retain the direct coordinator origin. */
  workerLinkOneWayDelayMs?: 0 | 25;
};

export interface FixtureWorkerLaunchOptions {
  bunExecutable: string;
  coordinatorUrl: string;
  sourceRoot: string;
  compileFixture(): void;
  fixtureExecutable: string;
  client: AuthorizedApiClient;
  paths: FixtureWorkerPaths;
  oneWayDelayMs?: 0 | 25;
  onWorkerStarted(service: RunningService): void;
  onLinkStarted(link: DelayedWorkerLink | undefined): void;
}

export async function startFixtureWorker(
  options: FixtureWorkerLaunchOptions,
): Promise<FixtureWorkerIdentity> {
  options.compileFixture();
  const link = options.oneWayDelayMs === undefined
    ? undefined
    : await startDelayedWorkerLink({
      targetUrl: options.coordinatorUrl,
      oneWayDelayMs: options.oneWayDelayMs,
    });
  options.onLinkStarted(link);
  const startWorker = createTerminalWorkerStarter(
    options.bunExecutable,
    link?.url ?? options.coordinatorUrl,
    options.sourceRoot,
  );
  const bootstrapToken = (
    await options.client.authMintBootstrap({ kind: "worker", label: options.paths.label })
  ).token;
  const service = startWorker({
    ...options.paths,
    bootstrapToken,
    shell: options.fixtureExecutable,
  });
  options.onWorkerStarted(service);
  const workerFp = await waitForTerminalWorkerRoutable(
    options.client,
    options.paths.label,
    options.paths.logPath,
  );
  return { workerFp, label: options.paths.label, home: options.paths.home, logPath: options.paths.logPath };
}
