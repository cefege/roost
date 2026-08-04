// Shared coding-agent presentation policy. Every surface uses the same derived
// level, priority, copy, and color; connection/offline state remains separate.

import type { AgentStatus } from "@roost/shared/wire";

export type AgentStatusLevel = "blocked" | "done" | "working" | "idle" | "unknown";

export interface AgentStatusPresentation {
  label: string;
  countLabel: string;
  tooltip: string;
  color: string;
  priority: number;
}

export const AGENT_STATUS_PRESENTATION: Readonly<Record<AgentStatusLevel, AgentStatusPresentation>> = {
  blocked: {
    label: "Needs input",
    countLabel: "needs input",
    tooltip: "The agent is waiting for your input",
    color: "var(--md-warning)",
    priority: 4,
  },
  done: {
    label: "Done",
    countLabel: "done",
    tooltip: "The agent finished since you last viewed this terminal",
    color: "var(--md-secondary)",
    priority: 3,
  },
  working: {
    label: "Working",
    countLabel: "working",
    tooltip: "The agent is working",
    color: "var(--md-primary)",
    priority: 2,
  },
  idle: {
    label: "Idle",
    countLabel: "idle",
    tooltip: "The agent is idle",
    color: "var(--md-success)",
    priority: 1,
  },
  unknown: {
    label: "Unknown",
    countLabel: "unknown",
    tooltip: "Agent status is unavailable",
    color: "var(--md-on-surface-variant)",
    priority: 0,
  },
};

/** Idle becomes Done only while its completion revision remains unseen. */
export function deriveAgentStatusLevel(
  status: AgentStatus | null | undefined,
  acknowledgedRevision = 0,
): AgentStatusLevel {
  if (!status) return "unknown";
  if (status.state === "blocked") return "blocked";
  if (status.state === "working") return "working";
  if (status.state === "idle" && status.completed_revision > acknowledgedRevision) return "done";
  return "idle";
}

export function agentStatusTooltip(
  status: AgentStatus,
  acknowledgedRevision = 0,
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
