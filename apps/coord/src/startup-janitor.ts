import { log } from "@roost/shared/log";
import { asWorkspaceId } from "@roost/shared/wire";
import { workspaceBus } from "./buses.ts";
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
    // Capture orphan ids BEFORE the delete so workspaceBus subscribers
    // (SPA sync stream) get the `deleted` deltas — without this, SPAs
    // that survive a coord restart see stale workspace rows in the
    // sidebar until the user reloads the tab.
    const orphanRows = await db
      .selectFrom("workspaces")
      .select("id")
      .where("id", "not in", db.selectFrom("workspace_sessions").select("workspace_id").distinct())
      .execute();
    const orphans = await db
      .deleteFrom("workspaces")
      .where("id", "not in", db.selectFrom("workspace_sessions").select("workspace_id").distinct())
      .executeTakeFirst();
    for (const row of orphanRows) {
      workspaceBus.publish({ kind: "deleted", id: asWorkspaceId(row.id as string) });
    }
    log.info("main", "janitor", {
      deleted_sessions: Number(closed?.numDeletedRows ?? 0),
      pruned_orphan_workspaces: Number(orphans?.numDeletedRows ?? 0),
    });
  } catch (error) {
    log.warn("main", "janitor_failed", { error: (error as Error).message });
  }
}
