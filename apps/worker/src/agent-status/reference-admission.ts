// Serializes durable agent-reference reports with complete boot reconciliation.
// A queued reporter is process-revalidated only after adoption/respawn/restore,
// while ordinary session lifecycle admission stays independent.
// The integration report path and the worker's own agent-exit clear both append
// through emitDurableAgentReference, so the reservation rules exist once.

import type { AgentConversationReferenceV1 } from "@roost/shared/agent-conversation-reference";
import type { SessionId } from "@roost/shared/wire";
import type { SessionEventSink } from "../event-sink.ts";

export type AgentReferenceEventSink = Pick<
  SessionEventSink,
  "reserveSessionEvent" | "releaseSessionEvent" | "emit"
>;

export class AgentReferenceAdmissionGate {
  private tail: Promise<void> = Promise.resolve();

  async runExclusive<Result>(operation: () => Promise<Result>): Promise<Result> {
    const predecessor = this.tail;
    const turn = Promise.withResolvers<void>();
    this.tail = predecessor.then(() => turn.promise);
    await predecessor;
    try {
      return await operation();
    } finally {
      turn.resolve();
    }
  }
}

/** Append one durable set or clear. The reservation is released only when the
 *  emit never reached the store: releasing a consumed reservation would punch a
 *  hole in the worker's client_seq run. */
export function emitDurableAgentReference(
  eventSink: AgentReferenceEventSink,
  sessionId: SessionId,
  reference: AgentConversationReferenceV1 | null,
): void {
  const reservation = eventSink.reserveSessionEvent("agent_reference");
  try {
    eventSink.emit({
      kind: "agent_reference",
      session_id: sessionId,
      reference,
      ts: Date.now(),
    }, reservation);
  } catch (error) {
    try {
      eventSink.releaseSessionEvent(reservation);
    } catch {
      // Append may have consumed the reservation before transport failed.
    }
    throw error;
  }
}
