// Durable Sync replay must yield often enough for live session events to preempt it.

import { describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { SessionEvent, asSessionId } from "@roost/shared/wire";
import { sessionBus } from "../src/buses.ts";
import { startSyncFeed, type SyncDashboardScope } from "../src/connect/sync-feed.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
const DASHBOARD = "sync-priority-dashboard";
const ORGANIZATION = "sync-priority-organization";

describe("Sync durable replay priority", () => {
  test("a queued live session event lands between sixteen-event replay batches", async () => {
    const dir = await mkdtemp(join(tmpdir(), "roost-sync-priority-"));
    const opened = openDb(join(dir, "coord.db"));
    try {
      await runMigrations(opened.sqlite);
      const now = Date.now();
      await opened.db.insertInto("organizations").values({
        id: ORGANIZATION,
        slug: "sync-priority",
        name: "Sync priority",
        status: "active",
        created_at_ms: now,
      }).execute();
      await opened.db.insertInto("dashboards").values({
        id: DASHBOARD,
        organization_id: ORGANIZATION,
        slug: "sync-priority",
        name: "Sync priority",
        status: "active",
        created_at_ms: now,
      }).execute();
      const seedEvent = {
        kind: "closed",
        session_id: "00000000-0000-4000-8000-000000000000",
        exit_code: null,
        ts: 1,
      };
      const seed = await opened.db.insertInto("events").values({
        kind: seedEvent.kind,
        dashboard_id: DASHBOARD,
        session_id: seedEvent.session_id,
        worker_fp: null,
        payload_json: JSON.stringify(seedEvent),
        ts: seedEvent.ts,
        client_seq: null,
      }).returning("id").executeTakeFirstOrThrow();
      const replayRows = Array.from({ length: 32 }, (_, index) => {
        const event = {
          kind: "closed",
          session_id: `00000000-0000-4000-8000-${String(index + 1).padStart(12, "0")}`,
          exit_code: null,
          ts: index + 2,
        };
        return {
          dashboard_id: DASHBOARD,
          kind: event.kind,
          session_id: event.session_id,
          worker_fp: null,
          payload_json: JSON.stringify(event),
          ts: event.ts,
          client_seq: null,
        };
      });
      await opened.db.insertInto("events").values(replayRows).execute();

      const order: string[] = [];
      let replayed = 0;
      let queuedLiveSession = false;
      const liveSessionId = "00000000-0000-4000-8000-999999999999";
      const liveEvent = SessionEvent.parse({
        kind: "closed",
        session_id: asSessionId(liveSessionId),
        exit_code: null,
        ts: 100,
      });
      const scope: SyncDashboardScope = {
        dashboardId: DASHBOARD,
        workerFps: new Set(),
        sessionIds: new Set([...replayRows.map((row) => row.session_id), liveSessionId]),
        workspaceIds: new Set(),
      };
      const deps = { db: opened.db } as unknown as ConnectDeps;
      const feed = startSyncFeed(deps, scope, Number(seed.id), (frame, meta) => {
        if (frame.frame.case !== "sessionEvent") return;
        if (meta?.sessionId === liveSessionId) {
          order.push("live-session");
          return;
        }
        order.push("replay");
        replayed += 1;
        if (!queuedLiveSession) {
          queuedLiveSession = true;
          queueMicrotask(() => sessionBus.publish(Object.assign(liveEvent, { _dashboard_id: DASHBOARD })));
        }
      }, null);
      try {
        await feed.backfill();
      } finally {
        feed.dispose();
      }

      expect(replayed).toBe(32);
      expect(order).toEqual([
        ...Array.from({ length: 16 }, () => "replay"),
        "live-session",
        ...Array.from({ length: 16 }, () => "replay"),
      ]);
    } finally {
      await opened.close();
      await rm(dir, { recursive: true, force: true });
    }
  });

  test("recovery dedupe keeps only its cutoff boundary", async () => {
    const dir = await mkdtemp(join(tmpdir(), "roost-sync-dedupe-"));
    const opened = openDb(join(dir, "coord.db"));
    try {
      await runMigrations(opened.sqlite);
      const now = Date.now();
      await opened.db.insertInto("organizations").values({
        id: ORGANIZATION,
        slug: "sync-dedupe",
        name: "Sync dedupe",
        status: "active",
        created_at_ms: now,
      }).execute();
      await opened.db.insertInto("dashboards").values({
        id: DASHBOARD,
        organization_id: ORGANIZATION,
        slug: "sync-dedupe",
        name: "Sync dedupe",
        status: "active",
        created_at_ms: now,
      }).execute();
      const event = SessionEvent.parse({
        kind: "closed",
        session_id: asSessionId("00000000-0000-4000-8000-111111111111"),
        exit_code: null,
        ts: now,
      });
      if (event.kind !== "closed") throw new Error("expected closed session event fixture");
      const insertEvent = async () => Number((await opened.db.insertInto("events").values({
        dashboard_id: DASHBOARD,
        kind: event.kind,
        session_id: event.session_id,
        worker_fp: null,
        payload_json: JSON.stringify(event),
        ts: event.ts,
        client_seq: null,
      }).returning("id").executeTakeFirstOrThrow()).id);
      const sinceEventId = await insertEvent();
      const boundaryEventId = await insertEvent();
      const deliveredIds: number[] = [];
      const scope: SyncDashboardScope = {
        dashboardId: DASHBOARD,
        workerFps: new Set(),
        sessionIds: new Set([event.session_id]),
        workspaceIds: new Set(),
      };
      const feed = startSyncFeed(
        { db: opened.db } as unknown as ConnectDeps,
        scope,
        sinceEventId,
        (frame) => {
          if (frame.frame.case === "sessionEvent") {
            deliveredIds.push(Number(frame.frame.value.eventId));
          }
        },
        null,
      );
      const publish = (eventId: number): void => {
        sessionBus.publish(Object.assign(event, {
          _dashboard_id: DASHBOARD,
          _event_id: eventId,
        }));
      };
      try {
        publish(boundaryEventId);
        await feed.backfill();
        publish(boundaryEventId);
        expect(deliveredIds).toEqual([boundaryEventId]);

        const firstPostRecoveryId = boundaryEventId + 1;
        for (let index = 0; index < 2_048; index += 1) {
          publish(firstPostRecoveryId + index);
        }
        const lastPostRecoveryId = firstPostRecoveryId + 2_047;
        publish(lastPostRecoveryId);
        expect(deliveredIds.slice(1)).toEqual([
          ...Array.from({ length: 2_048 }, (_, index) => firstPostRecoveryId + index),
          lastPostRecoveryId,
        ]);
      } finally {
        feed.dispose();
      }
    } finally {
      await opened.close();
      await rm(dir, { recursive: true, force: true });
    }
  });
});
