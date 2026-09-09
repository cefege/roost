// Formats and dispatches the agent-status read and wait commands.
// Read JSON remains an explicit stable projection; waits pin the exact current
// occupant and print only their terminal outcome.

import type { AgentStatusView } from "@roost/shared/proto/coordinator_pb";
import { AgentPromptWaitTimeoutMsSchema } from "@roost/shared/terminal-input";

export interface AgentStatusApiClient {
  agentStatusGet(request: { sessionId: string }): Promise<{ status?: AgentStatusView }>;
  agentStatusList(request: Record<string, never>): Promise<{ statuses: AgentStatusView[] }>;
  agentStatusWait(request: {
    sessionId: string;
    statusEpoch: string;
    occupantId: string;
    desiredStates: string[];
    timeoutMs: number;
  }): Promise<{ outcome: string }>;
}

export type AgentStatusLineWriter = (line: string) => void;
export type AgentStatusExitWriter = (code: number) => void;

type PublicAgentStatus = {
  session_id: string;
  agent_id: string;
  state: string;
  message: string | null;
  status_epoch: string | null;
  occupant_id: string | null;
  source: string | null;
  revision: number;
  completed_revision: number;
  updated_at: number;
  promptable: boolean;
};

const TSV_HEADER = [
  "session_id",
  "agent_id",
  "state",
  "message",
  "status_epoch",
  "occupant_id",
  "source",
  "revision",
  "completed_revision",
  "updated_at",
  "promptable",
].join("\t");

export async function dispatchAgentStatusApi(
  client: AgentStatusApiClient,
  verb: string,
  args: readonly string[],
  writeLine: AgentStatusLineWriter = (line) => console.log(line),
  writeExitCode: AgentStatusExitWriter = (code) => {
    process.exitCode = code;
  },
): Promise<boolean> {
  if (verb === "agent-status") {
    const sessionId = args[0];
    if (!sessionId || sessionId.startsWith("--")) {
      throw new Error("agent-status: missing <session>");
    }
    const response = await client.agentStatusGet({ sessionId });
    if (!response.status) {
      throw new Error("agent-status: coordinator returned an empty response");
    }
    printStatuses([projectAgentStatus(response.status)], args.includes("--json"), false, writeLine);
    return true;
  }

  if (verb === "agent-wait") {
    const parsed = parseAgentWaitArgs(args);
    const current = await client.agentStatusGet({ sessionId: parsed.sessionId });
    if (!current.status) {
      throw new Error("agent-wait: coordinator returned an empty status response");
    }
    if (!current.status.statusEpoch || !current.status.occupantId) {
      throw new Error("agent-wait: current agent status has no occupant identity");
    }
    const response = await client.agentStatusWait({
      sessionId: parsed.sessionId,
      statusEpoch: current.status.statusEpoch,
      occupantId: current.status.occupantId,
      desiredStates: parsed.desiredStates,
      timeoutMs: parsed.timeoutMs,
    });
    if (AGENT_WAIT_OUTCOMES[response.outcome] !== true) {
      throw new Error("agent-wait: coordinator returned an invalid outcome");
    }
    writeLine(response.outcome);
    if (response.outcome !== "matched") writeExitCode(1);
    return true;
  }

  if (verb !== "agents") return false;
  const response = await client.agentStatusList({});
  const statuses = [...response.statuses]
    .sort((left, right) => compareSessionIds(left.sessionId, right.sessionId))
    .map(projectAgentStatus);
  printStatuses(statuses, args.includes("--json"), true, writeLine);
  return true;
}

interface ParsedAgentWaitArgs {
  sessionId: string;
  desiredStates: string[];
  timeoutMs: number;
}

const AGENT_WAIT_STATES: Record<string, true | undefined> = {
  blocked: true,
  idle: true,
  working: true,
};
const AGENT_WAIT_OUTCOMES: Record<string, true | undefined> = {
  matched: true,
  timed_out: true,
  occupant_changed: true,
  session_closed: true,
};

function parseAgentWaitArgs(args: readonly string[]): ParsedAgentWaitArgs {
  const sessionId = args[0];
  if (!sessionId || sessionId.startsWith("--")) {
    throw new Error("agent-wait: missing <session>");
  }
  let untilValue: string | undefined;
  let timeoutValue: string | undefined;
  for (let index = 1; index < args.length; index += 1) {
    const argument = args[index]!;
    if (argument === "--until" || argument === "--timeout") {
      const value = args[index + 1];
      if (!value || value.startsWith("--")) {
        throw new Error(`agent-wait: ${argument} requires a value`);
      }
      if (argument === "--until") {
        if (untilValue !== undefined) throw new Error("agent-wait: duplicate --until");
        untilValue = value;
      } else {
        if (timeoutValue !== undefined) throw new Error("agent-wait: duplicate --timeout");
        timeoutValue = value;
      }
      index += 1;
    } else if (argument.startsWith("--until=")) {
      if (untilValue !== undefined) throw new Error("agent-wait: duplicate --until");
      untilValue = argument.slice("--until=".length);
    } else if (argument.startsWith("--timeout=")) {
      if (timeoutValue !== undefined) throw new Error("agent-wait: duplicate --timeout");
      timeoutValue = argument.slice("--timeout=".length);
    } else {
      throw new Error(`agent-wait: unexpected argument ${JSON.stringify(argument)}`);
    }
  }
  if (untilValue === undefined) throw new Error("agent-wait: missing --until");
  if (timeoutValue === undefined) throw new Error("agent-wait: missing --timeout");
  const desiredStates = parseAgentWaitStates(untilValue);
  return {
    sessionId,
    desiredStates,
    timeoutMs: parseAgentWaitDuration(timeoutValue),
  };
}

export function parseAgentWaitDuration(
  value: string,
  command = "agent-wait",
): number {
  const match = /^([1-9][0-9]*)(ms|s|m)$/.exec(value);
  if (!match) invalidAgentWaitDuration(command);
  const multiplier = match[2] === "m" ? 60_000n : match[2] === "s" ? 1_000n : 1n;
  const timeoutMs = Number(BigInt(match[1]!) * multiplier);
  if (!AgentPromptWaitTimeoutMsSchema.safeParse(timeoutMs).success) {
    invalidAgentWaitDuration(command);
  }
  return timeoutMs;
}

export function parseAgentWaitStates(
  value: string,
  command = "agent-wait",
): string[] {
  const desiredStates = value.split(",");
  if (
    desiredStates.length === 0
    || desiredStates.some((state) => AGENT_WAIT_STATES[state] !== true)
    || new Set(desiredStates).size !== desiredStates.length
  ) {
    throw new Error(`${command}: --until must be a unique comma-list of blocked,idle,working`);
  }
  return desiredStates;
}

function invalidAgentWaitDuration(command: string): never {
  throw new Error(
    `${command}: --timeout must be an integer duration from 1ms to 5m (for example 30s)`,
  );
}

function projectAgentStatus(status: AgentStatusView): PublicAgentStatus {
  return {
    session_id: status.sessionId,
    agent_id: status.agentId,
    state: status.state,
    message: status.message ?? null,
    status_epoch: status.statusEpoch ?? null,
    occupant_id: status.occupantId ?? null,
    source: status.source ?? null,
    revision: safeJsonInteger(status.revision, "revision"),
    completed_revision: safeJsonInteger(status.completedRevision, "completed_revision"),
    updated_at: status.updatedAt,
    promptable: status.promptable,
  };
}

function safeJsonInteger(value: bigint, field: string): number {
  const converted = Number(value);
  if (!Number.isSafeInteger(converted)) {
    throw new Error(`agent status ${field} exceeds the JSON integer range`);
  }
  return converted;
}

function compareSessionIds(left: string, right: string): number {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}

function printStatuses(
  statuses: readonly PublicAgentStatus[],
  json: boolean,
  list: boolean,
  writeLine: AgentStatusLineWriter,
): void {
  if (json) {
    const payload = list ? statuses : statuses[0];
    if (payload === undefined) throw new Error("agent-status: no status to print");
    const encoded = JSON.stringify(payload, null, 2);
    if (encoded === undefined) throw new Error("agent-status: status is not JSON-serializable");
    writeLine(encoded);
    return;
  }
  writeLine(TSV_HEADER);
  for (const status of statuses) writeLine(formatTsvRow(status));
}

function formatTsvRow(status: PublicAgentStatus): string {
  return [
    status.session_id,
    status.agent_id,
    status.state,
    status.message ?? "-",
    status.status_epoch ?? "-",
    status.occupant_id ?? "-",
    status.source ?? "legacy",
    status.revision,
    status.completed_revision,
    status.updated_at,
    status.promptable,
  ].map(escapeTsvCell).join("\t");
}

function escapeTsvCell(value: string | number | boolean): string {
  return String(value).replace(/[\x00-\x1f\x7f-\x9f\\]/g, (character) => {
    if (character === "\\") return "\\\\";
    if (character === "\t") return "\\t";
    if (character === "\r") return "\\r";
    if (character === "\n") return "\\n";
    return `\\x${character.charCodeAt(0).toString(16).padStart(2, "0")}`;
  });
}
