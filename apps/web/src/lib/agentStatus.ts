// Browser coding-agent identity and presentation policy. Identity comparisons
// use epoch plus occupant UUID; source is mutable provenance. Every visual
// surface shares the same derived level, priority, copy, and color.

import {
  isIdentifiedAgentStatus,
  type AgentOccupantId,
  type AgentStatus,
  type AgentStatusIdentity,
  type AgentStatusSource,
  type StatusEpoch,
} from "@roost/shared/wire";

type AgentStatusIdentityLike = {
  status_epoch?: StatusEpoch;
  occupant_id?: AgentOccupantId;
  source?: AgentStatusSource;
};

export type AgentStatusRevisionToken =
  | ({
    session_id: AgentStatus["session_id"];
    revision: number;
  } & AgentStatusIdentity)
  | {
    session_id: AgentStatus["session_id"];
    revision: number;
    status_epoch?: undefined;
    occupant_id?: undefined;
    source?: undefined;
  };

/** Source may change while the same process occupant remains current. */
export function sameAgentStatusOccupant(
  left: AgentStatusIdentityLike,
  right: AgentStatusIdentityLike,
): boolean {
  const leftIdentified = isIdentifiedAgentStatus(left);
  const rightIdentified = isIdentifiedAgentStatus(right);
  if (!leftIdentified || !rightIdentified) return leftIdentified === rightIdentified;
  return left.status_epoch === right.status_epoch
    && left.occupant_id === right.occupant_id;
}

export function agentStatusOccupantKey(status: AgentStatusIdentityLike): string | null {
  if (!isIdentifiedAgentStatus(status)) return null;
  return `${status.status_epoch}:${status.occupant_id}`;
}

export function agentStatusRevisionToken(
  status: AgentStatus,
): AgentStatusRevisionToken {
  const revision = status.revision;
  if (!isIdentifiedAgentStatus(status)) {
    return { session_id: status.session_id, revision };
  }
  return {
    session_id: status.session_id,
    revision,
    status_epoch: status.status_epoch,
    occupant_id: status.occupant_id,
    source: status.source,
  };
}

export function matchesAgentStatusRevisionToken(
  status: AgentStatus,
  token: AgentStatusRevisionToken,
): boolean {
  return status.session_id === token.session_id
    && status.revision === token.revision
    && sameAgentStatusOccupant(status, token);
}

export type AgentStatusLevel = "blocked" | "done" | "working" | "idle" | "unknown";

export interface AgentStatusPresentation {
  label: string;
  countLabel: string;
  tooltip: string;
  color: string;
  priority: number;
  dotStatus: "warn" | "ok" | "info" | "idle";
}

export const AGENT_STATUS_PRESENTATION: Readonly<Record<AgentStatusLevel, AgentStatusPresentation>> = {
  blocked: {
    label: "Needs input",
    countLabel: "needs input",
    tooltip: "The agent is waiting for your input",
    color: "var(--md-warning)",
    priority: 4,
    dotStatus: "warn",
  },
  done: {
    label: "Done",
    countLabel: "done",
    tooltip: "The agent finished since you last viewed this terminal",
    color: "var(--md-secondary)",
    priority: 3,
    dotStatus: "ok",
  },
  working: {
    label: "Working",
    countLabel: "working",
    tooltip: "The agent is working",
    color: "var(--md-primary)",
    priority: 2,
    dotStatus: "info",
  },
  idle: {
    label: "Idle",
    countLabel: "idle",
    tooltip: "The agent is idle",
    color: "var(--md-success)",
    priority: 1,
    dotStatus: "idle",
  },
  unknown: {
    label: "Unknown",
    countLabel: "unknown",
    tooltip: "Agent status is unavailable",
    color: "var(--md-on-surface-variant)",
    priority: 0,
    dotStatus: "idle",
  },
};

/** Idle becomes Done only while a real completion revision remains unseen. */
export function deriveAgentStatusLevel(
  status: AgentStatus | null | undefined,
  acknowledgedRevision?: number,
): AgentStatusLevel {
  if (!status) return "unknown";
  if (status.state === "blocked") return "blocked";
  if (status.state === "working") return "working";
  const seenRevision = acknowledgedRevision
    ?? (agentStatusOccupantKey(status) === null ? 0 : -1);
  if (
    status.state === "idle"
    && status.completed_revision > 0
    && status.completed_revision > seenRevision
  ) return "done";
  return "idle";
}

export function agentStatusTooltip(
  status: AgentStatus,
  acknowledgedRevision?: number,
): string {
  const presentation = AGENT_STATUS_PRESENTATION[
    deriveAgentStatusLevel(status, acknowledgedRevision)
  ];
  const message = status.message?.trim();
  return message ? `${presentation.tooltip}: ${message}` : presentation.tooltip;
}

export type AgentStatusCounts = Record<AgentStatusLevel, number>;

export interface AgentStatusRollup {
  level: AgentStatusLevel;
  counts: AgentStatusCounts;
  total: number;
}

export function foldAgentStatusLevels(levels: Iterable<AgentStatusLevel>): AgentStatusRollup {
  const counts: AgentStatusCounts = {
    blocked: 0,
    done: 0,
    working: 0,
    idle: 0,
    unknown: 0,
  };
  let level: AgentStatusLevel = "unknown";
  let total = 0;
  for (const value of levels) {
    counts[value]++;
    if (value !== "unknown") total++;
    if (AGENT_STATUS_PRESENTATION[value].priority > AGENT_STATUS_PRESENTATION[level].priority) {
      level = value;
    }
  }
  return { level, counts, total };
}

const COUNT_ORDER: readonly AgentStatusLevel[] = ["blocked", "working", "done", "idle"];

export function formatAgentStatusCounts(counts: AgentStatusCounts): string {
  return COUNT_ORDER
    .filter((level) => counts[level] > 0)
    .map((level) => `${counts[level]} ${AGENT_STATUS_PRESENTATION[level].countLabel}`)
    .join(" · ");
}
