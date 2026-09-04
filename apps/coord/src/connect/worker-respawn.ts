// Respawn-if-missing dispatch for open sessions after a worker reconnects.

import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  CoordWorkerDownSchema,
  DBrowserCommandSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { asSessionId, SessionKind, type ClientControlFrame } from "@roost/shared/wire";
import { log } from "@roost/shared/log";
import type { KyselyDB } from "../db/connection.ts";
import { connectWorkers, type WorkerHandle } from "./worker-registry.ts";

/**
 * Called after the worker hello grace period. Open terminal sessions are
 * revived at their saved cwd with the same session_id; the worker no-ops if
 * the sid is already live.
 */
export async function respawnMissingForWorker(
  db: KyselyDB,
  workerFp: string,
  handle: WorkerHandle,
): Promise<void> {
  if (!handle.ready || handle.revoked || connectWorkers.get(workerFp) !== handle) return;
  const dashboardId = handle.dashboardId;
  if (dashboardId === undefined) {
    log.warn("worker-service", "respawn_unscoped_worker_handle", { worker_fp: workerFp });
    return;
  }
  const rows = await db.selectFrom("sessions as session")
    .innerJoin("workers as worker", "worker.fp", "session.worker_fp")
    .select([
      "session.id as id",
      "session.kind as kind",
      "session.cwd as cwd",
    ])
    .where("session.worker_fp", "=", workerFp)
    .where("session.dashboard_id", "=", dashboardId)
    .where("session.status", "=", "open")
    .where("worker.dashboard_id", "=", dashboardId)
    .where("worker.deleted_at_ms", "is", null)
    .execute();
  if (rows.length === 0) return;
  log.info("worker-service", "respawn_missing_dispatch", { worker_fp: workerFp, count: rows.length });
  for (const row of rows) {
    if (!handle.ready || handle.revoked || connectWorkers.get(workerFp) !== handle) return;
    const requestId = randomUUID();
    // Kinds SessionKind knows about are recreated; a historical row carrying
    // any other value stays visible but is not respawned.
    const kind = SessionKind.safeParse(row.kind);
    if (!kind.success) {
      log.warn("worker-service", "respawn_unknown_kind", {
        worker_fp: workerFp, session_id: row.id, kind: row.kind,
      });
      continue;
    }
    const frame: ClientControlFrame = {
      kind: "respawn-if-missing",
      request_id: requestId,
      session_id: asSessionId(row.id),
      cwd: row.cwd,
      cols: 80,
      rows: 24,
    };
    try {
      // sendBrowserCmd helper expects a viewer_id; respawn-if-missing has
      // no human caller — use a synthetic "coord:respawn" tag.
      const bc = create(DBrowserCommandSchema, {
        browserId: "coord", viewerId: "coord:respawn",
        requestId, frameJson: JSON.stringify(frame),
      });
      handle.send(create(CoordWorkerDownSchema, {
        frame: { case: "browserCommand", value: bc },
      }));
    } catch (e) {
      log.warn("worker-service", "respawn_send_failed", {
        worker_fp: workerFp, session_id: row.id, error: String(e),
      });
    }
  }
}
