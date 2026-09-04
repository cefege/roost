// This module owns dashboard-scoped reads from the durable event log.
// Sync recovery reaches it through the stable event-log public facade.
// It depends only on Kysely and the shared SessionEvent wire shape.
// Ascending IDs and stable cursor cutoffs are recovery invariants.

import type { SessionEvent } from "@roost/shared/wire";
import type { KyselyDB } from "./db/connection.ts";

/** Read back events with id > sinceId for one dashboard's reconnect backfill. */
export async function getEventsSince(
  db: KyselyDB,
  dashboardId: string,
  sinceId: number,
  limit = 1000,
): Promise<Array<{ id: number; event: SessionEvent }>> {
  const rows = await db
    .selectFrom("events")
    .select(["id", "payload_json"])
    .where("dashboard_id", "=", dashboardId)
    .where("id", ">", sinceId)
    .orderBy("id", "asc")
    .limit(limit)
    .execute();
  return rows.map((row) => ({
    id: row.id as number,
    event: JSON.parse(row.payload_json as string) as SessionEvent,
  }));
}

/** Capture a dashboard-scoped durable recovery cutoff after live subscription. */
export async function getEventMaxId(db: KyselyDB, dashboardId: string): Promise<number> {
  const row = await db
    .selectFrom("events")
    .select(({ fn }) => fn.max<number>("id").as("max_id"))
    .where("dashboard_id", "=", dashboardId)
    .executeTakeFirst();
  return Number(row?.max_id ?? 0);
}

/** Page one dashboard-scoped stable recovery interval: cursor < id <= cutoff. */
export async function getEventsThrough(
  db: KyselyDB,
  dashboardId: string,
  cursor: number,
  cutoff: number,
  limit = 256,
): Promise<Array<{ id: number; event: SessionEvent }>> {
  const rows = await db
    .selectFrom("events")
    .select(["id", "payload_json"])
    .where("dashboard_id", "=", dashboardId)
    .where("id", ">", cursor)
    .where("id", "<=", cutoff)
    .orderBy("id", "asc")
    .limit(limit)
    .execute();
  return rows.map((row) => ({
    id: Number(row.id),
    event: JSON.parse(row.payload_json) as SessionEvent,
  }));
}
