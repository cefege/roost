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
import type { WorkerHandle } from "./worker-registry.ts";

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
  const rows = await db.selectFrom("sessions")
    .select(["id", "kind", "cwd"])
    .where("worker_fp", "=", workerFp)
    .where("status", "=", "open")
    .execute();
  if (rows.length === 0) return;
  log.info("worker-service", "respawn_missing_dispatch", { worker_fp: workerFp, count: rows.length });
  for (const row of rows) {
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
