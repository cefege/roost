// Retained-snapshot seeding for the Sync firehose: the legacy synchronous
// burst, the pair-request snapshot, and the v2 per-domain retained replay.
// Also owns the per-socket resource index those seeds are filtered by, plus
// the single query that loads it before upgrade. The live-subscription engine
// that drives these is sync-feed.ts.

import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  FirehoseFrameSchema, type FirehoseFrame, SessionPresenceSchema,
  WorkerRoutableFrameSchema, TerminalTitleFrameSchema, LastActivityFrameSchema,
  SyncDomain,
} from "@roost/shared/proto/sync_pb";
import { listRoutableFps } from "./worker-service.ts";
import { getTitleSnapshot } from "../terminal-title-hub.ts";
import { getLastActivitySnapshot } from "../last-activity-hub.ts";
import { getAgentStatusSnapshot } from "../agent-status-hub.ts";
import { terminalViewerProjection } from "./terminal-view-hub.ts";
import { log } from "@roost/shared/log";
import { agentStatusFrame, type SyncFeedFrameMeta } from "./sync-feed-frames.ts";
import type { UiStateOwner } from "./ui-state-owner.ts";
import type { KyselyDB } from "../db/connection.ts";
import { uiStateSeedFrames } from "./sync-feed-ui.ts";

export interface SyncResourceIndex {
  /** Non-null only for a read-only worker Sync socket, whose sets stay limited
   * to that worker's own resources. Null admits every install resource. */
  readonly ownerWorkerFp: string | null;
  /** Mutable for the socket lifetime: durable session events extend or remove
   * it before subsequent title/presence/cell fan-out. */
  readonly sessionIds: Set<string>;
  /** Mutable for worker-registration deltas; used to intersect routability. */
  readonly workerFps: Set<string>;
  readonly workspaceIds: Set<string>;
}

/** Load persisted runtime ownership before the socket is upgraded. A browser
 * indexes the whole install; a worker caller is narrowed to its own resources
 * so its read-only firehose never carries another worker's state. */
export async function loadSyncResourceIndex(
  db: KyselyDB,
  ownerWorkerFp: string | null = null,
): Promise<SyncResourceIndex> {
  const workerQuery = db.selectFrom("workers").select("fp")
    .where("deleted_at_ms", "is", null);
  const sessionQuery = db.selectFrom("sessions").select("id");
  const workspaceQuery = db.selectFrom("workspaces").select("id");
  const [workers, sessions, workspaces] = await Promise.all([
    (ownerWorkerFp === null
      ? workerQuery
      : workerQuery.where("fp", "=", ownerWorkerFp)).execute(),
    (ownerWorkerFp === null
      ? sessionQuery
      : sessionQuery.where("worker_fp", "=", ownerWorkerFp)).execute(),
    (ownerWorkerFp === null
      ? workspaceQuery
      : workspaceQuery.where("worker_fp", "=", ownerWorkerFp)).execute(),
  ]);
  return {
    ownerWorkerFp,
    workerFps: new Set(workers.map((row) => row.fp)),
    sessionIds: new Set(sessions.map((row) => row.id)),
    workspaceIds: new Set(workspaces.map((row) => row.id)),
  };
}

/** What a retained seed needs from the feed it is seeding: whether this socket
 * negotiated v2, whether the feed has been disposed since seeding began, and
 * where a retained frame goes. */
export interface SyncFeedSeedContext {
  readonly v2: boolean;
  isDisposed(): boolean;
  push(frame: FirehoseFrame, meta: SyncFeedFrameMeta): void;
}

export function* retainedSeedFrames(
  scope: SyncResourceIndex,
  uiStates: UiStateOwner,
  browserUi: boolean,
): Generator<FirehoseFrame> {
  // Live routable worker membership is volatile, so seed it before the
  // per-session snapshots below.
  yield create(FirehoseFrameSchema, {
    frame: {
      case: "workerRoutable",
      value: create(WorkerRoutableFrameSchema, {
        fps: listRoutableFps().filter((fp) => scope.workerFps.has(fp)),
      }),
    },
  });

  for (const { session_id, title } of getTitleSnapshot()) {
    if (!scope.sessionIds.has(session_id)) continue;
    yield create(FirehoseFrameSchema, {
      frame: {
        case: "terminalTitle",
        value: create(TerminalTitleFrameSchema, { sessionId: session_id, title }),
      },
    });
  }

  for (const { session_id, ts_ms } of getLastActivitySnapshot()) {
    if (!scope.sessionIds.has(session_id)) continue;
    yield create(FirehoseFrameSchema, {
      frame: {
        case: "lastActivity",
        value: create(LastActivityFrameSchema, { sessionId: session_id, tsMs: ts_ms }),
      },
    });
  }

  for (const status of getAgentStatusSnapshot()) {
    if (scope.sessionIds.has(status.session_id)) yield agentStatusFrame(status);
  }
  if (browserUi) yield* uiStateSeedFrames(uiStates);
}


export async function seedDomain(
  ctx: SyncFeedSeedContext,
  scope: SyncResourceIndex,
  domain: SyncDomain,
  sessionIds?: ReadonlySet<string>,
): Promise<void> {
  if (!ctx.v2 || ctx.isDisposed()) return;
  const retained = (frame: FirehoseFrame, sessionId?: string): void => {
    if (ctx.isDisposed()) return;
    ctx.push(frame, { domain, lane: "retained", sessionId, beforeBuffered: true });
  };

  if (domain === SyncDomain.WORKERS) {
    const fps = listRoutableFps().filter((fp) => scope.workerFps.has(fp));
    const snapshotId = randomUUID();
    const chunkSize = 256;
    const chunkCount = Math.max(1, Math.ceil(fps.length / chunkSize));
    for (let chunkIndex = 0; chunkIndex < chunkCount; chunkIndex += 1) {
      retained(create(FirehoseFrameSchema, {
        frame: {
          case: "workerRoutable",
          value: create(WorkerRoutableFrameSchema, {
            fps: fps.slice(chunkIndex * chunkSize, (chunkIndex + 1) * chunkSize),
            snapshotId,
            chunkIndex,
            chunkCount,
          }),
        },
      }));
    }
    return;
  }
  if (domain !== SyncDomain.TERMINAL) return;

  for (const { session_id, title } of getTitleSnapshot()) {
    if (!scope.sessionIds.has(session_id)) continue;
    if (sessionIds && !sessionIds.has(session_id)) continue;
    retained(create(FirehoseFrameSchema, {
      frame: {
        case: "terminalTitle",
        value: create(TerminalTitleFrameSchema, { sessionId: session_id, title }),
      },
    }), session_id);
  }
  for (const { session_id, ts_ms } of getLastActivitySnapshot()) {
    if (!scope.sessionIds.has(session_id)) continue;
    if (sessionIds && !sessionIds.has(session_id)) continue;
    retained(create(FirehoseFrameSchema, {
      frame: {
        case: "lastActivity",
        value: create(LastActivityFrameSchema, { sessionId: session_id, tsMs: ts_ms }),
      },
    }), session_id);
  }
  for (const status of getAgentStatusSnapshot()) {
    if (!scope.sessionIds.has(status.session_id)) continue;
    if (sessionIds && !sessionIds.has(status.session_id)) continue;
    retained(agentStatusFrame(status), status.session_id);
  }
  for (const [sessionId, viewers] of terminalViewerProjection()) {
    if (!scope.sessionIds.has(sessionId)) continue;
    const entries = [...viewers.entries()].map(([fp, geometry]) => ({
      fp,
      viewerKey: fp,
      cols: geometry.cols,
      rows: geometry.rows,
      lastMs: Date.now(),
    }));
    if (sessionIds && !sessionIds.has(sessionId)) continue;
    retained(create(FirehoseFrameSchema, {
      frame: {
        case: "sessionPresence",
        value: create(SessionPresenceSchema, {
          sessionId,
          payloadJson: JSON.stringify({
            kind: "viewers",
            fps: entries.map((entry) => entry.fp),
            entries,
          }),
        }),
      },
    }), sessionId);
  }
}
