// Owns SessionsList database selection plus public and private row conversion.
// Private columns are selected only for the authenticated owning-worker branch.
// Persisted opaque JSON is strictly reparsed and failures never echo its value.

import { sessionToProto } from "@roost/shared/wire/session-proto";
import { Session, type SessionStatus as SessionStatusValue } from "@roost/shared/wire";
import {
  AgentConversationReferenceV1Schema,
  AgentConversationRecoveryMetadataSchema,
} from "@roost/shared/agent-conversation-reference";
import { sessionRecoveryMetadataToProto } from "@roost/shared/agent-conversation-reference-proto";
import { safeJsonParse } from "@roost/shared/json";
import type { SessionRecoveryMetadata } from "@roost/shared/proto/coordinator_pb";
import type { Session as ProtoSession } from "@roost/shared/proto/wire_pb";
import type { Selectable } from "kysely";
import type { SessionsTable } from "../db/schema.ts";
import { SESSION_COLUMNS } from "../event-projection.ts";
import type { KyselyDB } from "../db/connection.ts";

export const SESSION_RECOVERY_COLUMNS = [
  "agent_reference_json",
  "agent_reference_client_seq",
] as const;
const WORKER_SESSION_COLUMNS = [
  ...SESSION_COLUMNS,
  ...SESSION_RECOVERY_COLUMNS,
] as const;

export type PublicSessionProjectionRow = Pick<
  Selectable<SessionsTable>,
  (typeof SESSION_COLUMNS)[number]
>;
export type WorkerSessionRecoveryRow = PublicSessionProjectionRow & Pick<
  Selectable<SessionsTable>,
  (typeof SESSION_RECOVERY_COLUMNS)[number]
>;

export interface SessionsListProjection {
  sessionIds: string[];
  sessions: ProtoSession[];
  recoveryMetadata: SessionRecoveryMetadata[];
}

export async function readSessionsListProjection(
  db: KyselyDB,
  options: {
    workerFp: string;
    status: SessionStatusValue | null;
    includeRecovery: true;
  } | {
    workerFp?: string;
    status: SessionStatusValue | null;
    includeRecovery: false;
  },
): Promise<SessionsListProjection> {
  if (options.includeRecovery && !options.workerFp) {
    throw new Error("private session recovery query requires an owning worker");
  }
  let query = db.selectFrom("sessions");
  if (options.workerFp) query = query.where("worker_fp", "=", options.workerFp);
  if (options.status !== null) query = query.where("status", "=", options.status);

  if (options.includeRecovery) {
    const rows = await query.select([...WORKER_SESSION_COLUMNS]).execute();
    return {
      sessionIds: rows.map((row) => row.id),
      sessions: rows.map(sessionRowToProto),
      recoveryMetadata: rows.map(sessionRecoveryRowToProto),
    };
  }
  const rows = await query.select([...SESSION_COLUMNS]).execute();
  return {
    sessionIds: rows.map((row) => row.id),
    sessions: rows.map(sessionRowToProto),
    recoveryMetadata: [],
  };
}

export function sessionRowToProto(row: PublicSessionProjectionRow) {
  return sessionToProto(Session.parse({
    id: row.id,
    worker_fp: row.worker_fp,
    channel: row.channel,
    kind: row.kind,
    cwd: row.cwd,
    workspace_id: row.workspace_id ?? null,
    status: row.status,
    created_at: row.created_at,
    closed_at: row.closed_at ?? null,
    custom_title: row.custom_title ?? null,
    git_branch: row.git_branch ?? null,
    git_remote: row.git_remote ?? null,
    pr_number: row.pr_number ?? null,
    pr_state: (row.pr_state ?? null) as never,
    pr_checks: (row.pr_checks ?? null) as never,
    pr_url: row.pr_url ?? null,
    ports: row.ports_json
      ? safeJsonParse<number[]>(row.ports_json, [], "session.ports")
      : [],
    spawn_cwd: row.spawn_cwd ?? null,
  }));
}

export function sessionRecoveryRowToProto(
  row: WorkerSessionRecoveryRow,
): SessionRecoveryMetadata {
  try {
    const agentReference = row.agent_reference_json === null
      ? null
      : AgentConversationReferenceV1Schema.parse(
        JSON.parse(row.agent_reference_json),
      );
    return sessionRecoveryMetadataToProto(
      AgentConversationRecoveryMetadataSchema.parse({
        session_id: row.id,
        agent_reference: agentReference,
        agent_reference_client_seq: row.agent_reference_client_seq ?? 0,
      }),
    );
  } catch {
    throw new Error("stored agent conversation recovery metadata is invalid");
  }
}
