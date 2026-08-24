// Boot-time purge of durable rows that cannot represent live state after a
// restart: deletes closed sessions and workspaces orphaned by them.
// Load-bearing ordering: runs BEFORE sync feeds install bus listeners, so no
// workspaceBus deltas are published here — reconnecting SPAs learn about the
// pruning only from their sync-feed seed snapshot. Never touches 'open'
// sessions; dead open ones are ghost-closed by worker snapshot reconcile.
import { log } from "@roost/shared/log";
import type { KyselyDB } from "./db/connection.ts";

/** Purge durable rows that cannot represent live coordinator state after boot. */
export async function runStartupJanitor(db: KyselyDB): Promise<void> {
  try {
    // Closed sessions are DELETED, not parked (no "closed" limbo). Purge any
    // already-closed rows (legacy 'closed' data + safety net). NEVER touch
    // 'open' rows here — a live long-running terminal must never be deleted by
    // a janitor; truly-dead open sessions are reconciled by the worker
    // snapshot's ghost-close on reconnect, not by a wall-clock age cutoff.
    const closed = await db
      .deleteFrom("sessions")
      .where("status", "=", "closed")
      .executeTakeFirst();
    await db
      .deleteFrom("workspace_sessions")
      .where("workspace_id", "not in", db
        .selectFrom("workspace_sessions as ws")
        .innerJoin("sessions as s", "s.id", "ws.session_id")
        .where("s.status", "=", "open").select("ws.workspace_id"))
      .execute();
    // Orphaned workspaces are deleted WITHOUT workspaceBus deltas: this runs
    // before the sync feeds install their bus listeners, so any publish here
    // is a structurally guaranteed no-op. Surviving SPAs instead learn about
    // pruned rows from the sync-feed seed snapshot they receive on
    // (re)connect — that seed is what actually protects the sidebar.
    const orphans = await db
      .deleteFrom("workspaces")
      .where("id", "not in", db.selectFrom("workspace_sessions").select("workspace_id").distinct())
      .executeTakeFirst();
    log.info("main", "janitor", {
      deleted_sessions: Number(closed?.numDeletedRows ?? 0),
      pruned_orphan_workspaces: Number(orphans?.numDeletedRows ?? 0),
    });
  } catch (error) {
    log.warn("main", "janitor_failed", { error: (error as Error).message });
  }
}
