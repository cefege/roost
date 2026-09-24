// Respawn-if-missing dispatch for open sessions after a worker reconnects.
// Called by worker-conn.ts once the hello grace period ends; it reads the
// durable session rows, the write gate, and the terminal-view hub's effective
// geometry so a revived PTY comes back at the size its viewers are already
// showing rather than at a fixed default.

import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  CoordWorkerDownSchema,
  DBrowserCommandSchema,
} from "@roost/protocol/proto/worker_transport_pb";
import { asSessionId, SessionKind, type ClientControlFrame } from "@roost/protocol/wire";
import { log } from "@roost/observability/log";
import type { KyselyDB } from "../db/connection.ts";
import type { CoordinatorWriteGate } from "../coordinator-write-gate.ts";
import { connectWorkers, type WorkerHandle } from "./worker-registry.ts";
import { terminalViewSnapshot } from "../terminal/view/terminal-view-hub.ts";

/** Nothing is watching, so the PTY is framed at the conventional default and
 *  the first view to attach reframes it. Respawning at this size while views
 *  DO exist makes every attached TUI redraw twice. */
const RESPAWN_UNWATCHED_COLS = 80;
const RESPAWN_UNWATCHED_ROWS = 24;

/**
 * Called after the worker hello grace period. Open terminal sessions are
 * revived at their saved cwd with the same session_id; the worker no-ops if
 * the sid is already live.
 *
 * The gate wait is not optional: a reconnect must not recreate a channel while
 * the coordinator is proving the keeper empty for an update.
 */
export async function respawnMissingForWorker(
  db: KyselyDB,
  workerFp: string,
  handle: WorkerHandle,
  writeGate: CoordinatorWriteGate,
): Promise<void> {
  const lease = await writeGate.acquireAfterExclusive();
  try {
    if (!handle.ready || handle.revoked || connectWorkers.get(workerFp) !== handle) return;
    const rows = await db.selectFrom("sessions as session")
      .innerJoin("workers as worker", "worker.fp", "session.worker_fp")
      .select([
        "session.id as id",
        "session.kind as kind",
        "session.cwd as cwd",
      ])
      .where("session.worker_fp", "=", workerFp)
      .where("session.status", "=", "open")
      .where("worker.deleted_at_ms", "is", null)
      .execute();
    if (rows.length === 0) return;
    for (const row of rows) {
      if (!handle.ready || handle.revoked || connectWorkers.get(workerFp) !== handle) return;
      const requestId = randomUUID();
      const kind = SessionKind.safeParse(row.kind);
      if (!kind.success) {
        log.warn("worker-service", "respawn_unknown_kind", {
          worker_fp: workerFp,
          session_id: row.id,
          kind: row.kind,
        });
        continue;
      }
      const effective = terminalViewSnapshot(row.id)?.effective ?? null;
      const dispatchCols = effective?.cols ?? RESPAWN_UNWATCHED_COLS;
      const dispatchRows = effective?.rows ?? RESPAWN_UNWATCHED_ROWS;
      log.info("worker-service", "respawn_missing_dispatch", {
        worker_fp: workerFp,
        count: rows.length,
        session_id: row.id,
        cols: dispatchCols,
        rows: dispatchRows,
        geometry_source: effective ? "terminal_view" : "unwatched_default",
      });
      const frame: ClientControlFrame = {
        kind: "respawn-if-missing",
        request_id: requestId,
        session_id: asSessionId(row.id),
        cwd: row.cwd,
        cols: dispatchCols,
        rows: dispatchRows,
      };
      try {
        const browserCommand = create(DBrowserCommandSchema, {
          browserId: "coord",
          viewerId: "coord:respawn",
          requestId,
          frameJson: JSON.stringify(frame),
        });
        handle.send(create(CoordWorkerDownSchema, {
          frame: { case: "browserCommand", value: browserCommand },
        }));
      } catch (error) {
        log.warn("worker-service", "respawn_send_failed", {
          worker_fp: workerFp,
          session_id: row.id,
          error: String(error),
        });
      }
    }
  } finally {
    lease.release();
  }
}
