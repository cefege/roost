// Canonical typed catalog of Roost-owned agent integration assets.
// The source installer and embed generator iterate this same list; runtime
// validation rejects missing, duplicate, or unexpected generated entries.

export type AgentIntegrationAssetId =
  | "omp-status"
  | "omp-reference"
  | "pi-status";
export type AgentIntegrationRuntime = "omp" | "pi";

export interface AgentIntegrationAssetSpec {
  readonly id: AgentIntegrationAssetId;
  readonly runtime: AgentIntegrationRuntime;
  readonly sourcePath: string;
  readonly installFilename: string;
  readonly ownershipMarker: string;
}

export interface EmbeddedAgentIntegrationAsset {
  readonly id: AgentIntegrationAssetId;
  readonly content: string;
}

export const AGENT_INTEGRATION_ASSET_SPECS = [
  {
    id: "omp-status",
    runtime: "omp",
    sourcePath: "integrations/omp/roost-agent-state.ts",
    installFilename: "roost-omp-agent-state.ts",
    ownershipMarker: "ROOST_INTEGRATION_ID=omp",
  },
  {
    id: "omp-reference",
    runtime: "omp",
    sourcePath: "integrations/omp/roost-agent-reference.ts",
    installFilename: "roost-omp-agent-reference.ts",
    ownershipMarker: "ROOST_INTEGRATION_ID=omp-reference",
  },
  {
    id: "pi-status",
    runtime: "pi",
    sourcePath: "integrations/pi/roost-agent-state.ts",
    installFilename: "roost-pi-agent-state.ts",
    ownershipMarker: "ROOST_INTEGRATION_ID=pi",
  },
] as const satisfies readonly AgentIntegrationAssetSpec[];

export const RETIRED_AGENT_INTEGRATION_SPECS = [
  {
    runtime: "omp",
    installFilename: "roost-omp-session-api.ts",
    ownershipMarker: "ROOST_INTEGRATION_ID=omp",
  },
] as const satisfies readonly Pick<
  AgentIntegrationAssetSpec,
  "runtime" | "installFilename" | "ownershipMarker"
>[];
