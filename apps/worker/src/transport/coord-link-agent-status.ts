// Ordered, coalescing outbox for volatile identified agent status. It keeps a
// bounded ordered history of possibly-lost retirements plus the latest active
// occupant per session, so reconnect repair covers every remotely possible
// prefix without allowing native backpressure to invert replacement edges.
import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WAgentStatusSchema,
  type CoordWorkerUp,
} from "@roost/shared/proto/worker_transport_pb";
import {
  isIdentifiedAgentStatus,
  type AgentStatusUpdate,
} from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { WORKER_SNAPSHOT_MAX_SESSIONS } from "./coord-link-constants.ts";

interface EncodedAgentStatus {
  status: AgentStatusUpdate;
  occupantKey: string;
  bytes: Uint8Array;
}

export interface CoordLinkAgentStatusOutbox {
  send(status: AgentStatusUpdate, canWriteDirect: boolean): boolean;
  drain(): void;
  disconnect(): void;
  hasPending(): boolean;
  clear(): void;
}

interface CoordLinkAgentStatusOptions {
  encodeUpstream(frame: CoordWorkerUp): Uint8Array | null;
  tryWriteEncoded(bytes: Uint8Array): boolean;
  scheduleDrain(): void;
}

const AGENT_STATUS_REPAIR_CAP = WORKER_SNAPSHOT_MAX_SESSIONS;

function compactPending(
  possiblySentKey: string | undefined,
  pending: readonly EncodedAgentStatus[],
): EncodedAgentStatus[] {
  let currentKey = possiblySentKey;
  let retirement: EncodedAgentStatus | undefined;

  for (const item of pending) {
    if (item.status.active) {
      currentKey = item.occupantKey;
      continue;
    }
    if (possiblySentKey !== undefined && item.occupantKey === possiblySentKey) {
      retirement = item;
    }
    if (item.occupantKey === currentKey) {
      currentKey = undefined;
    }
  }

  const repaired: EncodedAgentStatus[] = [];
  if (possiblySentKey !== undefined && currentKey !== possiblySentKey && retirement) {
    repaired.push(retirement);
  }
  if (currentKey !== undefined) {
    for (let index = pending.length - 1; index >= 0; index--) {
      const item = pending[index]!;
      if (item.status.active && item.occupantKey === currentKey) {
        repaired.push(item);
        break;
      }
    }
  }
  return repaired;
}

export function createCoordLinkAgentStatusOutbox(
  options: CoordLinkAgentStatusOptions,
): CoordLinkAgentStatusOutbox {
  const pendingBySession = new Map<string, EncodedAgentStatus[]>();
  const repairReplayBySession = new Map<string, EncodedAgentStatus[]>();
  const possiblySentBySession = new Map<string, string>();
  const retirementRepairs: EncodedAgentStatus[] = [];

  function rememberRetirement(item: EncodedAgentStatus): void {
    const existingIndex = retirementRepairs.findIndex(
      (retirement) => retirement.occupantKey === item.occupantKey,
    );
    if (existingIndex >= 0) {
      retirementRepairs[existingIndex] = item;
      return;
    }
    retirementRepairs.push(item);
    if (retirementRepairs.length <= AGENT_STATUS_REPAIR_CAP) return;
    const evicted = retirementRepairs.shift();
    if (
      evicted
      && possiblySentBySession.get(evicted.status.session_id) === evicted.occupantKey
    ) {
      possiblySentBySession.delete(evicted.status.session_id);
    }
  }

  function encodeStatus(status: AgentStatusUpdate): EncodedAgentStatus | null {
    if (!isIdentifiedAgentStatus(status)) {
      diag("transport.frame_dropped", {
        reason: "unidentified_agent_status",
        kind: "agentStatus",
      });
      log.warn("coord-link", "unidentified_agent_status_dropped", {
        session_id: status.session_id,
        agent_id: status.agent_id,
        revision: status.revision,
      });
      return null;
    }
    const bytes = options.encodeUpstream(create(CoordWorkerUpSchema, {
      frame: {
        case: "agentStatus",
        value: create(WAgentStatusSchema, {
          sessionId: status.session_id,
          agentId: status.agent_id,
          state: status.state,
          message: status.message,
          revision: BigInt(status.revision),
          completedRevision: BigInt(status.completed_revision),
          updatedAt: status.updated_at,
          active: status.active,
          statusEpoch: status.status_epoch,
          occupantId: status.occupant_id,
          source: status.source,
        }),
      },
    }));
    if (!bytes) return null;
    return {
      status,
      occupantKey: `${status.status_epoch}:${status.occupant_id}`,
      bytes,
    };
  }

  function noteWritten(item: EncodedAgentStatus): void {
    const sessionId = item.status.session_id;
    if (item.status.active) {
      possiblySentBySession.set(sessionId, item.occupantKey);
    } else if (possiblySentBySession.get(sessionId) === item.occupantKey) {
      rememberRetirement(item);
    }
  }

  function queue(item: EncodedAgentStatus): void {
    const sessionId = item.status.session_id;
    const pending = pendingBySession.get(sessionId) ?? [];
    pending.push(item);
    const repaired = compactPending(possiblySentBySession.get(sessionId), pending);
    if (repaired.length > 0) pendingBySession.set(sessionId, repaired);
    else pendingBySession.delete(sessionId);
    options.scheduleDrain();
  }

  return {
    send(status, canWriteDirect) {
      const item = encodeStatus(status);
      if (!item) return false;
      const directlyRelevant = item.status.active
        || possiblySentBySession.get(item.status.session_id) === item.occupantKey;
      if (
        canWriteDirect
        && directlyRelevant
        && repairReplayBySession.size === 0
        && pendingBySession.size === 0
        && options.tryWriteEncoded(item.bytes)
      ) {
        noteWritten(item);
        return true;
      }
      queue(item);
      return true;
    },
    drain() {
      for (const [sessionId, pending] of repairReplayBySession) {
        while (pending.length > 0) {
          const item = pending[0]!;
          if (!options.tryWriteEncoded(item.bytes)) return;
          noteWritten(item);
          pending.shift();
        }
        repairReplayBySession.delete(sessionId);
      }
      for (const [sessionId, pending] of pendingBySession) {
        while (pending.length > 0) {
          const item = pending[0]!;
          if (!options.tryWriteEncoded(item.bytes)) return;
          noteWritten(item);
          pending.shift();
        }
        pendingBySession.delete(sessionId);
      }
    },
    disconnect() {
      repairReplayBySession.clear();
      for (const retirement of retirementRepairs) {
        const sessionId = retirement.status.session_id;
        const replay = repairReplayBySession.get(sessionId) ?? [];
        replay.push(retirement);
        repairReplayBySession.set(sessionId, replay);
      }
    },
    hasPending() {
      return repairReplayBySession.size > 0 || pendingBySession.size > 0;
    },
    clear() {
      pendingBySession.clear();
      repairReplayBySession.clear();
      possiblySentBySession.clear();
      retirementRepairs.length = 0;
    },
  };
}
