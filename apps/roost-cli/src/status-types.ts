// Status data shapes live together so probes and rendering share one contract.
// Keeping these types dependency-free prevents the extracted status concerns
// from reaching through one another just to describe the same report.

import type { KeeperRuntimeObservationV1 } from "@roost/shared/keeper-update";

export interface WorkerStatus {
  fingerprint: string;
  label: string;
  os: string;
  reachableAddr: string | null;
  gitSha: string | null;
  keeperRuntime: KeeperRuntimeObservationV1 | null;
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

export interface StatusEndpointOverride {
  origin: string;
}

export interface StatusReport {
  coordAgentLoaded: boolean;
  workerAgentLoaded: boolean;
  coord: { reachable: boolean; gitSha: string | null };
  workers: WorkerStatus[];
  endpoint: EndpointStatus;
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
