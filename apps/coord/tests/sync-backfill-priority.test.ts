// Durable Sync replay must yield often enough for live session events to preempt it.

import { describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { SessionEvent, asSessionId } from "@roost/shared/wire";
import { sessionBus } from "../src/buses.ts";
import { startSyncFeed } from "../src/connect/sync-feed.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";

describe("Sync durable replay priority", () => {
  test("a queued live session event lands between sixteen-event replay batches", async () => {
    const dir = await mkdtemp(join(tmpdir(), "roost-sync-priority-"));
    const opened = openDb(join(dir, "coord.db"));
    try {
      await runMigrations(opened.sqlite);
      const seedEvent = {
        kind: "closed",
        session_id: "00000000-0000-4000-8000-000000000000",
        exit_code: null,
        ts: 1,
      };
      const seed = await opened.db.insertInto("events").values({
        kind: seedEvent.kind,
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
      const deps = { db: opened.db } as unknown as ConnectDeps;
      const feed = startSyncFeed(deps, Number(seed.id), (frame, meta) => {
        if (frame.frame.case !== "sessionEvent") return;
        if (meta?.sessionId === liveSessionId) {
          order.push("live-session");
          return;
        }
        order.push("replay");
        replayed += 1;
        if (!queuedLiveSession) {
          queuedLiveSession = true;
          queueMicrotask(() => sessionBus.publish(liveEvent));
        }
      });
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
});
