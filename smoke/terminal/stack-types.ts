// Public lifecycle contracts for the terminal smoke stack.
// stack.ts creates the isolated processes; fixtures and peer specs consume these
// stable options without reaching into child environments or mutable launch state.
// Direct packet fault control stays a disposable harness-only dependency.

import type { AuthorizedApiClient } from "../../apps/roost-cli/src/api.ts";
import type { DelayedWorkerLink } from "./delayed-worker-link.ts";
import type { PtyFixtureWorkerStartOptions } from "./stack-fixture-worker.ts";
import type { HeldDirectInput } from "./stack-direct-input-hold.ts";
import type { PeerFaultOfferKind } from "./stack-peer-fault-control.ts";
import type { PeerFaultMalformedPacketKind } from "./stack-peer-fault-control.ts";
import type { TerminalWorkerPeerPortRange } from "./stack-worker-runtime.ts";

export type TerminalTestWorker = {
  workerFp: string;
  label: string;
  home: string;
  logPath: string;
};

export type TerminalReleaseCheckout = {
  /** Checkout the process runs from. */
  sourceRoot: string;
  /** Build identity it reports; deploy admission and convergence compare it. */
  gitSha: string;
};

export type TerminalPeerSmokeOptions = {
  /** Explicit coordinator enablement; omitted keeps non-peer smoke on Sync/loopback. */
  readonly coordinatorEnabled?: boolean;
  /** Explicitly empty omits STUN discovery without disabling direct host candidates. */
  readonly coordinatorStunUrls?: readonly string[];
  /** Explicit worker native-peer enablement; omitted keeps non-peer smoke isolated. */
  readonly workerEnabled?: boolean;
  /** Install source-worker-only peer fault callbacks; unavailable for packaged workers. */
  readonly enableFaults?: boolean;
  readonly workerBindAddress?: string;
  readonly workerPortRange?: TerminalWorkerPeerPortRange;
  /** Disable only the browser's local bootstrap probe for remote-peer tests. */
  readonly disableLoopbackProbe?: boolean;
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
  /** Bind the coordinator to a reserved local HTTP origin with production CSP. */
  localFirst?: boolean;
  /** Exact compiled `roost` binary used for worker processes instead of source Bun. */
  workerExecutable?: string;
  /** Exact compiled `roost` binary used for the coordinator instead of source Bun. */
  coordExecutable?: string;
  terminalPeer?: TerminalPeerSmokeOptions;
};

export interface TerminalPeerFaults {
  /** Hold one authenticated peer input after reassembly and before its PTY write. */
  holdNextDirectInput(sessionId: string): Promise<HeldDirectInput>;
  setPeerPacketBlackhole(workerLabel: string, enabled: boolean): Promise<void>;
  dropNextDirectInputResult(workerLabel: string): Promise<void>;
  advanceGrantClock(workerLabel: string, milliseconds: number): Promise<void>;
  shrinkGrantForSession(workerLabel: string, sessionId: string): Promise<number>;
  holdNextDirectHistoryResponse(
    workerLabel: string,
    sessionId: string,
  ): Promise<{ release(): Promise<void>; drop(): Promise<void> }>;
  injectMalformedDirectPacket(
    workerLabel: string,
    kind: PeerFaultMalformedPacketKind,
  ): Promise<void>;
  setDirectHistoryPaused(workerLabel: string, paused: boolean): Promise<void>;
  holdKeeperAdmission(
    workerLabel: string,
    sessionId: string,
  ): Promise<{ release(): Promise<void> }>;
  dropNextDirectRetire(workerLabel: string): Promise<void>;
  armNextOfferFault(workerLabel: string, fault: PeerFaultOfferKind): Promise<void>;
}

export type TerminalTestStack = {
  baseUrl: string;
  workerFp: string;
  workerHome: string;
  coordLogPath: string;
  workerLogPath: string;

  ptyFixtureWorkerLogPath: string;
  secondPtyFixtureWorkerLogPath: string;
  secondWorkerLogPath: string;
  /** The fixture context must deliberately reject only local bootstrap probing. */
  disableLoopbackProbe: boolean;
  // The authorized client the harness already had to mint to bootstrap the
  // worker. Exposed so callers don't build a second (unauthorized) one.
  client: AuthorizedApiClient;
  // Lazily start one independent worker with its own HOME, data, key, log, and
  // keeper. Repeated calls return the same running worker.
  startSecondWorker(): Promise<TerminalTestWorker>;
  /** Lazily start a worker whose shell is the compiled portable PTY fixture. */
  startPtyFixtureWorker(options?: PtyFixtureWorkerStartOptions): Promise<TerminalTestWorker>;
  /** Lazily start an independent compiled fixture worker with separate keeper state. */
  startSecondPtyFixtureWorker(options?: PtyFixtureWorkerStartOptions): Promise<TerminalTestWorker>;
  /** Delayed coordinator link the PTY fixture worker dials, when one was requested. */
  ptyFixtureWorkerLink: DelayedWorkerLink | null;
  /** Stop or relaunch only the coordinator child; worker, keeper and PTYs stay live. */
  stopCoordinator(): Promise<void>;
  startCoordinator(): Promise<void>;
  /** Source-worker-only peer fault controls; null for ordinary and packaged stacks. */
  peerFaults: TerminalPeerFaults | null;
  /** Worker-served local UI origin for a fingerprint this stack started. */
  localUiUrl(workerFp: string): string;
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
