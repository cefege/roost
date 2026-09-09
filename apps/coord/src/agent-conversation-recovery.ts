// Owns the coordinator's private agent-conversation recovery projection.
// Durable event transactions call this after inserting an agent_reference row.
// Worker client sequence, not event time or volatile agent status, orders updates.

import { AgentConversationReferenceV1Schema } from "@roost/shared/agent-conversation-reference";
import type { SessionEvent } from "@roost/shared/wire";
import type { KyselyDB } from "./db/connection.ts";

export async function projectAgentConversationReference(
  tx: KyselyDB,
  event: Extract<SessionEvent, { kind: "agent_reference" }>,
  clientSeq: number,
  workerFp: string,
): Promise<void> {
  if (!Number.isSafeInteger(clientSeq) || clientSeq <= 0) {
    throw new Error("agent conversation reference requires a valid worker sequence");
  }
  const referenceJson = event.reference === null
    ? null
    : JSON.stringify(AgentConversationReferenceV1Schema.parse(event.reference));
  await tx.updateTable("sessions")
    .set({
      agent_reference_json: referenceJson,
      agent_reference_client_seq: clientSeq,
    })
    .where("id", "=", event.session_id)
    .where("worker_fp", "=", workerFp)
    .where((expression) => expression.or([
      expression("agent_reference_client_seq", "is", null),
      expression("agent_reference_client_seq", "<", clientSeq),
    ]))
    .execute();
}
