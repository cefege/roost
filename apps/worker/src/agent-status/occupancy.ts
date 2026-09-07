// Private occupancy model behind the agent-status registry: the per-process
// candidates a session can offer, the effective occupant published from them,
// and why a candidate went away. The registry owns every transition over these
// shapes; nothing here reaches the wire, so process IDs stay worker-private.

import type {
  AgentOccupantId,
  AgentRuntimeState,
  AgentStatusSource,
} from "@roost/shared/wire";
import type { BuiltinAgentId } from "./process-scan.ts";

export interface ProcessCandidate {
  agentId: BuiltinAgentId;
  processId: number;
  processKey: string;
  state: AgentRuntimeState;
}

export interface IntegrationCandidate extends ProcessCandidate {
  message?: string;
  seq: number;
  leaseUntil: number;
}

/** A screen candidate carries whether the manifest matched a rule marked
 * `visible_blocker`: an on-screen prompt is direct evidence a human is being
 * waited on, and it may correct an integration that is not a full-lifecycle
 * state authority. */
export interface ScreenCandidate extends ProcessCandidate {
  visibleBlocker: boolean;
}

export interface EffectiveEntry extends ProcessCandidate {
  message?: string;
  source: AgentStatusSource;
  occupantId: AgentOccupantId;
  revision: number;
  completedRevision: number;
  updatedAt: number;
  /** False once the occupant's last candidate disappeared and this row only
   * survives to carry a completion no viewer has acknowledged. A dead occupant
   * can neither back a prompt proof nor be reclaimed by a later observation of
   * the same numeric PID. */
  occupantLive: boolean;
}

export interface SessionEntry {
  integration?: IntegrationCandidate;
  integrationSeqByProcess: Map<string, number>;
  screen?: ScreenCandidate;
  screenAbsenceObserved: boolean;
  effective?: EffectiveEntry;
  retiredProcessKeys: Set<string>;
}

/** Why an occupant's last candidate went away. An exit keeps an unacknowledged
 * completion alive as a forced idle transition — an agent that finishes and
 * then leaves is still done, and only a viewer clears that. An integration's
 * explicit `active: false` is instead the withdrawal verb: it retires the row,
 * as does closing the session. */
export type CandidateLoss = "exit" | "withdrawn";

export function processKey(agentId: BuiltinAgentId, processId: number): string {
  return `${agentId}:${processId}`;
}
