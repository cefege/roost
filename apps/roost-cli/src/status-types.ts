// Status data shapes live together so probes and rendering share one contract.
// Keeping these types dependency-free prevents the extracted status concerns
// from reaching through one another just to describe the same report.

export interface WorkerStatus {
  fingerprint: string;
  label: string;
  os: string;
  reachableAddr: string | null;
  gitSha: string | null;
  keeperState: "current" | "unknown" | "stale";
  keeperBuild: string | null;
  lastSeenMs: number;
  ageMs: number;
  stale: boolean;
}

/** Coordinator-move state as the coord persists it (coord-move/state.ts).
 *  Structural on purpose — roost-cli imports @roost/shared only, never coord. */
export interface HandoffStatus {
  role: "SOURCE" | "TARGET";
  phase: string;
  handoffId: string;
  sourceUrl: string;
  targetUrl: string;
}

export interface TailscaleStatus {
  required: boolean;
  state: string;
  fqdn: string | null;
  running: boolean;
}

export interface StatusEndpointOverride {
  mode: "automatic" | "explicit";
  origin: string;
}

export interface StatusReport {
  tailscale: TailscaleStatus;
  coordAgentLoaded: boolean;
  workerAgentLoaded: boolean;
  coord: { reachable: boolean; gitSha: string | null };
  workers: WorkerStatus[];
  tlsMode: "tailscale-serve" | "direct" | "missing";
  url: string | null;
  handoff: HandoffStatus | null;
}

export interface ResolvedStatusEndpoint {
  mode: StatusEndpointOverride["mode"];
  origin: string | null;
  healthUrl: string | null;
  tailscale: TailscaleStatus;
}

export interface StatusEndpointResolverOptions {
  platform?: NodeJS.Platform;
  override?: StatusEndpointOverride;
  resolveTailscale?: () => { state: string; fqdn: string | null };
}
