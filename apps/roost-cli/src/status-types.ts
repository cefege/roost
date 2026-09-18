// Status data shapes live together so probes and rendering share one contract.
// Keeping these types dependency-free prevents the extracted status concerns
// from reaching through one another just to describe the same report.

import type { KeeperRuntimeObservationV1 } from "@roost/shared/keeper-update";
import type { TerminalCoreCapacityReport } from "@roost/shared/terminal-core-capacity";

export interface WorkerStatus {
  fingerprint: string;
  label: string;
  os: string;
  reachableAddr: string | null;
  gitSha: string | null;
  keeperRuntime: KeeperRuntimeObservationV1 | null;
  /** Absent for a status fixture or a coordinator predating capacity reporting. */
  terminalCoreCapacity?: TerminalCoreCapacityReport | null;
  coordinatorOpenSessionIds: readonly string[];
  lastSeenMs: number;
  ageMs: number;
  stale: boolean;
}

/** The operator-declared front door. Roost installs no proxy, tunnel, or
 *  certificate, so the only claim it can make is whether that URL answers. */
export interface EndpointStatus {
  publicUrl: string | null;
  answers: boolean;
}

/** What the installed coordinator does with a page request. The SPA source it
 *  picked is its own startup line (`spa_source`); the CLI reports only what it
 *  can observe, because a released install serves an embedded build this
 *  process cannot see. */
export interface SpaStatus {
  /** HEAD `/` on the coordinator's own listener answered 200. Null when there
   *  was no listener to ask. */
  serves: boolean | null;
  /** The dist the installed service stamped, for the remedy. */
  webDistPath: string | null;
  /** Whether that stamped path holds a servable `index.html` right now. */
  webDistPresent: boolean;
}

export interface StatusEndpointOverride {
  origin: string;
}

export interface StatusReport {
  coordAgentLoaded: boolean;
  workerAgentLoaded: boolean;
  coord: { reachable: boolean; gitSha: string | null };
  workers: WorkerStatus[];
  endpoint: EndpointStatus;
  spa: SpaStatus;
}

export interface ResolvedStatusEndpoint {
  /** Front door the installed coordinator was told to advertise. */
  publicUrl: string | null;
  /** The coordinator's own listener on this host, when one is installed. */
  coordUrl: string | null;
}

export interface StatusEndpointResolverOptions {
  platform?: NodeJS.Platform;
  override?: StatusEndpointOverride;
}
