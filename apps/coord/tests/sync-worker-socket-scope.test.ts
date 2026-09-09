// A worker Sync socket is a read-only firehose over its own resources: the
// persisted index narrows to that worker and the feed drops every durable,
// live and retained frame keyed to another worker's session. A browser socket
// carries no owner worker and indexes the whole install instead.
// Depends on real migrations, the resource-index query and the live buses.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { FirehoseFrame } from "@roost/shared/proto/sync_pb";
import { sessionBus, titleBus } from "../src/buses.ts";
import { loadSyncResourceIndex, startSyncFeed } from "../src/connect/sync-feed.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";

const workerA = "a".repeat(64);
const workerB = "b".repeat(64);
const sessionA = "10000000-0000-4000-8000-000000000001";
const sessionB = "20000000-0000-4000-8000-000000000002";
const workspaceA = "30000000-0000-4000-8000-000000000001";
const workspaceB = "40000000-0000-4000-8000-000000000002";
let db: KyselyDB;
let closeDb: () => Promise<void>;
let workdir: string;

const closed = (sessionId: string, ts: number) => ({
  kind: "closed",
  session_id: sessionId,
  exit_code: 0,
  ts,
}) as never;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-sync-worker-scope-"));
  const opened = openDb(join(workdir, "coord.db"));
  db = opened.db;
  closeDb = opened.close;
  await runMigrations(opened.sqlite);
  const { dashboardId } = ensureSelfHostedTenant(opened.sqlite, {
    backfillLegacyScopes: false,
  });
  const now = Date.now();
  await db.insertInto("workers").values([
    { fp: workerA, dashboard_id: dashboardId, label: "A", os: "linux", registered_at_ms: now, last_seen_ms: now },
    { fp: workerB, dashboard_id: dashboardId, label: "B", os: "linux", registered_at_ms: now, last_seen_ms: now },
  ]).execute();
  await db.insertInto("sessions").values([
    { id: sessionA, dashboard_id: dashboardId, worker_fp: workerA, channel: 1, kind: "shell", cwd: "/a", status: "open", created_at: now },
    { id: sessionB, dashboard_id: dashboardId, worker_fp: workerB, channel: 2, kind: "shell", cwd: "/b", status: "open", created_at: now },
  ]).execute();
  await db.insertInto("workspaces").values([
    {
      id: workspaceA, dashboard_id: dashboardId, worker_fp: workerA, name: "A",
      folder_path: "/a", color: null, position: 0, version: 1,
      created_at_ms: now, updated_at_ms: now,
    },
    {
      id: workspaceB, dashboard_id: dashboardId, worker_fp: workerB, name: "B",
      folder_path: "/b", color: null, position: 1, version: 1,
      created_at_ms: now, updated_at_ms: now,
    },
  ]).execute();
  await db.insertInto("events").values([
    { dashboard_id: dashboardId, kind: "closed", session_id: sessionA, worker_fp: workerA, payload_json: JSON.stringify(closed(sessionA, 1)), ts: 1, client_seq: null },
    { dashboard_id: dashboardId, kind: "closed", session_id: sessionB, worker_fp: workerB, payload_json: JSON.stringify(closed(sessionB, 2)), ts: 2, client_seq: null },
    { dashboard_id: dashboardId, kind: "closed", session_id: sessionA, worker_fp: workerA, payload_json: JSON.stringify(closed(sessionA, 3)), ts: 3, client_seq: null },
  ]).execute();
});

afterAll(async () => {
  await closeDb();
  if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
});

test("a worker index holds only its own resources while a browser index spans the install", async () => {
  const workerIndex = await loadSyncResourceIndex(db, workerA);
  expect(workerIndex.ownerWorkerFp).toBe(workerA);
  expect([...workerIndex.workerFps]).toEqual([workerA]);
  expect([...workerIndex.sessionIds]).toEqual([sessionA]);
  expect([...workerIndex.workspaceIds]).toEqual([workspaceA]);

  const browserIndex = await loadSyncResourceIndex(db);
  expect(browserIndex.ownerWorkerFp).toBeNull();
  expect([...browserIndex.workerFps].sort()).toEqual([workerA, workerB].sort());
  expect([...browserIndex.sessionIds].sort()).toEqual([sessionA, sessionB].sort());
  expect([...browserIndex.workspaceIds].sort()).toEqual([workspaceA, workspaceB].sort());
});

test("a worker feed excludes another worker's durable and live session frames", async () => {
  const scope = await loadSyncResourceIndex(db, workerA);
  const frames: FirehoseFrame[] = [];
  const feed = startSyncFeed(
    { db } as ConnectDeps,
    scope,
    1,
    (frame) => frames.push(frame),
    null,
    false,
  );
  try {
    await feed.backfill();
    titleBus.publish({ session_id: sessionA, title: "A" });
    titleBus.publish({ session_id: sessionB, title: "B" });
    sessionBus.publish(closed(sessionA, 4));
    sessionBus.publish(closed(sessionB, 5));
    await Promise.resolve();
    const sessionFrames = frames.filter((frame) => frame.frame.case === "sessionEvent");
    expect(sessionFrames).toHaveLength(2);
    expect(sessionFrames.every((frame) =>
      frame.frame.case === "sessionEvent"
      && frame.frame.value.kind.case === "closed"
      && frame.frame.value.kind.value.sessionId === sessionA,
    )).toBe(true);
    const titles = frames.filter((frame) => frame.frame.case === "terminalTitle");
    expect(titles).toHaveLength(1);
    expect(titles[0]?.frame.case === "terminalTitle" && titles[0].frame.value.sessionId)
      .toBe(sessionA);
  } finally {
    feed.dispose();
  }
});
