// This module owns reads from the durable event log's single global stream.
// Sync recovery reaches it through the stable event-log public facade.
// It depends only on Kysely and the shared SessionEvent wire shape.
// Ascending IDs and stable cursor cutoffs are recovery invariants.

import type { SessionEvent } from "@roost/shared/wire";
import type { KyselyDB } from "./db/connection.ts";
import { PRIVATE_SESSION_EVENT_KIND } from "./session-event-visibility.ts";

/** Read back events with id > sinceId for a reconnect backfill. */
export async function getEventsSince(
  db: KyselyDB,
  sinceId: number,
  limit = 1000,
): Promise<Array<{ id: number; event: SessionEvent }>> {
  const rows = await db
    .selectFrom("events")
    .select(["id", "payload_json"])
    .where("id", ">", sinceId)
    .where("kind", "!=", PRIVATE_SESSION_EVENT_KIND)
    .orderBy("id", "asc")
    .limit(limit)
    .execute();
  return rows.map((row) => ({
    id: row.id as number,
    event: JSON.parse(row.payload_json as string) as SessionEvent,
  }));
}

/** Capture a durable recovery cutoff after live subscription. */
export async function getEventMaxId(db: KyselyDB): Promise<number> {
  const row = await db
    .selectFrom("events")
    .select(({ fn }) => fn.max<number>("id").as("max_id"))
    .where("kind", "!=", PRIVATE_SESSION_EVENT_KIND)
    .executeTakeFirst();
  return Number(row?.max_id ?? 0);
}

/** Page one stable recovery interval: cursor < id <= cutoff. */
export async function getEventsThrough(
  db: KyselyDB,
  cursor: number,
  cutoff: number,
  limit = 256,
): Promise<Array<{ id: number; event: SessionEvent }>> {
  const rows = await db
    .selectFrom("events")
    .select(["id", "payload_json"])
    .where("id", ">", cursor)
    .where("id", "<=", cutoff)
    .where("kind", "!=", PRIVATE_SESSION_EVENT_KIND)
    .orderBy("id", "asc")
    .limit(limit)
    .execute();
  return rows.map((row) => ({
    id: Number(row.id),
    event: JSON.parse(row.payload_json) as SessionEvent,
  }));
}
