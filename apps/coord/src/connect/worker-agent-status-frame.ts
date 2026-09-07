// Decodes one authenticated worker agent-status protobuf into the shared wire
// contract before handing it to the coordinator status hub. Optional identity
// fields remain absent when an older worker omits them; partial triples fail
// the hub's shared-schema validation instead of becoming legacy provenance.

import type { WAgentStatus } from "@roost/shared/proto/worker_transport_pb";
import { handleWorkerAgentStatus, type AgentStatusAcceptance } from "../agent-status-hub.ts";

export function dispatchWorkerAgentStatusFrame(
  workerFp: string,
  status: WAgentStatus,
): AgentStatusAcceptance {
  return handleWorkerAgentStatus(workerFp, {
    session_id: status.sessionId,
    agent_id: status.agentId,
    state: status.state,
    message: status.message,
    revision: Number(status.revision),
    completed_revision: Number(status.completedRevision),
    updated_at: status.updatedAt,
    active: status.active,
    status_epoch: status.statusEpoch,
    occupant_id: status.occupantId,
    source: status.source,
    occupant_exited: status.occupantExited,
  });
}
