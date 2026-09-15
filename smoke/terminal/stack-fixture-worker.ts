// Fixture-worker launch support owns the optional delayed coordinator link and fixture shell identity.
// The terminal stack supplies isolated paths and captures each started child for teardown.
// It reuses the ordinary worker starter so fixture workers retain the real auth and keeper lifecycle.

import type { AuthorizedApiClient } from "../../apps/roost-cli/src/api.ts";
import type { CoordWorkerUp } from "../../apps/shared/src/gen/roost/v1/worker_transport_pb.ts";
import { startDelayedWorkerLink, type DelayedWorkerLink } from "./delayed-worker-link.ts";
import {
  createTerminalWorkerStarter,
  waitForTerminalWorkerRoutable,
} from "./stack-worker-runtime.ts";
import type { WorkerLocalUi } from "./stack-local-ui.ts";
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
  workerLinkOneWayDelayMs?: 0 | 25 | 200;
  /** Drop selected complete worker→coordinator protobuf frames after WebSocket framing. */
  workerFrameFilter?: (frame: CoordWorkerUp) => boolean;
};

export interface FixtureWorkerLaunchOptions {
  bunExecutable: string;
  coordinatorUrl: string;
  sourceRoot: string;
  compileFixture(): void;
  fixtureExecutable: string;
  client: AuthorizedApiClient;
  paths: FixtureWorkerPaths;
  /** Loopback origin this worker serves; reserved before the coordinator was
   *  launched, so the coordinator already allowlists it. */
  localUi: WorkerLocalUi;
  oneWayDelayMs?: 0 | 25 | 200;
  workerFrameFilter?: (frame: CoordWorkerUp) => boolean;
  onWorkerStarted(service: RunningService): void;
  onLinkStarted(link: DelayedWorkerLink | undefined): void;
}

export async function startFixtureWorker(
  options: FixtureWorkerLaunchOptions,
): Promise<FixtureWorkerIdentity> {
  options.compileFixture();
  const link = options.oneWayDelayMs === undefined && options.workerFrameFilter === undefined
    ? undefined
    : await startDelayedWorkerLink({
      targetUrl: options.coordinatorUrl,
      oneWayDelayMs: options.oneWayDelayMs ?? 0,
      workerFrameFilter: options.workerFrameFilter,
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
  await options.localUi.release();
  const service = startWorker({
    ...options.paths,
    bootstrapToken,
    shell: options.fixtureExecutable,
    localUiBind: options.localUi.bind,
  });
  options.onWorkerStarted(service);
  const workerFp = await waitForTerminalWorkerRoutable(
    options.client,
    options.paths.label,
    options.paths.logPath,
  );
  options.localUi.record(workerFp);
  return { workerFp, label: options.paths.label, home: options.paths.home, logPath: options.paths.logPath };
}

export function createFixtureWorkerStarter(
  launch: FixtureWorkerLaunchOptions,
): (options?: PtyFixtureWorkerStartOptions) => Promise<FixtureWorkerIdentity> {
  let start: Promise<FixtureWorkerIdentity> | undefined;
  let oneWayDelayMs: 0 | 25 | 200 | undefined;
  let workerFrameFilter: PtyFixtureWorkerStartOptions["workerFrameFilter"];
  return (options: PtyFixtureWorkerStartOptions = {}): Promise<FixtureWorkerIdentity> => {
    if (start) {
      if (
        oneWayDelayMs !== options.workerLinkOneWayDelayMs
        || workerFrameFilter !== options.workerFrameFilter
      ) {
        return Promise.reject(new Error("fixture worker link options cannot change after startup"));
      }
      return start;
    }
    oneWayDelayMs = options.workerLinkOneWayDelayMs;
    workerFrameFilter = options.workerFrameFilter;
    start = startFixtureWorker({ ...launch, oneWayDelayMs, workerFrameFilter });
    return start;
  };
}
