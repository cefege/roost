// Formats and dispatches dashboard-scoped agent-status read commands.
// Called by api.ts before the remaining API verb switch.
// Keeps JSON stable by projecting protobuf messages field by field.

import type { AgentStatusView } from "@roost/shared/proto/coordinator_pb";

export interface AgentStatusReadClient {
  agentStatusGet(request: { sessionId: string }): Promise<{ status?: AgentStatusView }>;
  agentStatusList(request: Record<string, never>): Promise<{ statuses: AgentStatusView[] }>;
}

export type AgentStatusLineWriter = (line: string) => void;

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
  client: AgentStatusReadClient,
  verb: string,
  args: readonly string[],
  writeLine: AgentStatusLineWriter = (line) => console.log(line),
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

  if (verb !== "agents") return false;
  const response = await client.agentStatusList({});
  const statuses = [...response.statuses]
    .sort((left, right) => compareSessionIds(left.sessionId, right.sessionId))
    .map(projectAgentStatus);
  printStatuses(statuses, args.includes("--json"), true, writeLine);
  return true;
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
