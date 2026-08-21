// Retained-snapshot seeding for the Sync firehose: the legacy synchronous
// burst, the pair-request snapshot, and the v2 per-domain retained replay.
// Split out of handlers-streaming.ts; the live-subscription engine that drives
// these is sync-feed.ts.

import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  FirehoseFrameSchema, type FirehoseFrame, SessionPresenceSchema,
  WorkerRoutableFrameSchema, TerminalTitleFrameSchema, LastActivityFrameSchema,
  UiStateFrameSchema, SyncDomain,
} from "@roost/shared/proto/sync_pb";
import {
  PairRequestDeltaProtoSchema, PairRequestsSnapshotSchema,
} from "@roost/shared/proto/events_pb";
import { PairRequestSchema } from "@roost/shared/proto/wire_pb";
import type { KyselyDB } from "../db/connection.ts";
import { listRoutableFps } from "./worker-service.ts";
import { getTitleSnapshot } from "../terminal-title-hub.ts";
import { getUiStateSnapshot } from "./handlers-ui.ts";
import { getLastActivitySnapshot } from "../last-activity-hub.ts";
import { getAgentStatusSnapshot } from "../agent-status-hub.ts";
import { terminalViewerProjection } from "./terminal-view-hub.ts";
import { log } from "@roost/shared/log";
import { agentStatusFrame, type SyncFeedFrameMeta } from "./sync-feed-frames.ts";

/** What a retained seed needs from the feed it is seeding: whether this socket
 *  negotiated v2, whether the feed has been disposed since seeding began, and
 *  where a retained frame goes. */
export interface SyncFeedSeedContext {
  readonly v2: boolean;
  isDisposed(): boolean;
  push(frame: FirehoseFrame, meta: SyncFeedFrameMeta): void;
}

export function* retainedSeedFrames(): Generator<FirehoseFrame> {
  // Live routable worker membership is volatile, so seed it before the
  // per-session snapshots below.
  yield create(FirehoseFrameSchema, {
    frame: {
      case: "workerRoutable",
      value: create(WorkerRoutableFrameSchema, { fps: listRoutableFps() }),
    },
  });

  for (const { session_id, title } of getTitleSnapshot()) {
    yield create(FirehoseFrameSchema, {
      frame: {
        case: "terminalTitle",
        value: create(TerminalTitleFrameSchema, { sessionId: session_id, title }),
      },
    });
  }

  for (const { session_id, ts_ms } of getLastActivitySnapshot()) {
    yield create(FirehoseFrameSchema, {
      frame: {
        case: "lastActivity",
        value: create(LastActivityFrameSchema, { sessionId: session_id, tsMs: ts_ms }),
      },
    });
  }

  for (const status of getAgentStatusSnapshot()) yield agentStatusFrame(status);

  for (const { fp, tabId, state } of getUiStateSnapshot()) {
    yield create(FirehoseFrameSchema, {
      frame: {
        case: "uiState",
        value: create(UiStateFrameSchema, { fp, tabId, state }),
      },
    });
  }
}

export async function loadPairSnapshot(db: KyselyDB): Promise<FirehoseFrame | null> {
  try {
    const pending = await db.selectFrom("pair_requests")
      .select(["ephemeral_id", "label", "created_at_ms"])
      .where("status", "=", "pending").execute();
    return create(FirehoseFrameSchema, {
      frame: {
        case: "pairRequestDelta",
        value: create(PairRequestDeltaProtoSchema, {
          kind: {
            case: "snapshot",
            value: create(PairRequestsSnapshotSchema, {
              pending: pending.map((row) => create(PairRequestSchema, {
                ephemeralId: row.ephemeral_id,
                label: row.label,
                createdAtMs: BigInt(row.created_at_ms),
              })),
            }),
          },
        }),
      },
    });
  } catch (e) {
    log.warn("connect.sync", "pair_seed_failed", { error: String(e) });
    return null;
  }
}

export async function seedDomain(
  ctx: SyncFeedSeedContext,
  domain: SyncDomain,
  sessionIds?: ReadonlySet<string>,
): Promise<void> {
  if (!ctx.v2 || ctx.isDisposed()) return;
  const retained = (frame: FirehoseFrame, sessionId?: string): void => {
    if (ctx.isDisposed()) return;
    ctx.push(frame, { domain, lane: "retained", sessionId, beforeBuffered: true });
  };

  if (domain === SyncDomain.WORKERS) {
    const fps = listRoutableFps();
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
    if (sessionIds && !sessionIds.has(session_id)) continue;
    retained(create(FirehoseFrameSchema, {
      frame: {
        case: "terminalTitle",
        value: create(TerminalTitleFrameSchema, { sessionId: session_id, title }),
      },
    }), session_id);
  }
  for (const { session_id, ts_ms } of getLastActivitySnapshot()) {
    if (sessionIds && !sessionIds.has(session_id)) continue;
    retained(create(FirehoseFrameSchema, {
      frame: {
        case: "lastActivity",
        value: create(LastActivityFrameSchema, { sessionId: session_id, tsMs: ts_ms }),
      },
    }), session_id);
  }
  for (const status of getAgentStatusSnapshot()) {
    if (sessionIds && !sessionIds.has(status.session_id)) continue;
    retained(agentStatusFrame(status), status.session_id);
  }
  for (const [sessionId, viewers] of terminalViewerProjection()) {
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
