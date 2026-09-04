import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { sql } from "kysely";
import { create } from "@bufbuild/protobuf";
import { TerminalViewCommandSchema, type FirehoseFrame } from "@roost/shared/proto/sync_pb";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { sessionBus, titleBus } from "../src/buses.ts";
import { startSyncFeed, type SyncDashboardScope } from "../src/connect/sync-feed.ts";
import { TerminalViewHub } from "../src/connect/terminal-view-hub.ts";
import { processInputControl } from "../src/connect/input-control.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-service.ts";
import type { ConnectDeps } from "../src/connect/router.ts";

const dashboardA = "sync-isolation-a";
const dashboardB = "sync-isolation-b";
const workerA = "a".repeat(64);
const workerB = "b".repeat(64);
const sessionA = "10000000-0000-4000-8000-000000000001";
const sessionB = "20000000-0000-4000-8000-000000000002";
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
  workdir = mkdtempSync(join(tmpdir(), "roost-dashboard-sync-"));
  const opened = openDb(join(workdir, "coord.db"));
  db = opened.db;
  closeDb = opened.close;
  await runMigrations(opened.sqlite);
  const now = Date.now();
  await db.insertInto("organizations").values({
    id: "sync-isolation-org", slug: "sync-isolation", name: "Sync isolation",
    status: "active", created_at_ms: now,
  }).execute();
  await db.insertInto("dashboards").values([
    { id: dashboardA, organization_id: "sync-isolation-org", slug: "a", name: "A", status: "active", created_at_ms: now },
    { id: dashboardB, organization_id: "sync-isolation-org", slug: "b", name: "B", status: "active", created_at_ms: now },
  ]).execute();
  await db.insertInto("workers").values([
    { fp: workerA, dashboard_id: dashboardA, label: "A", os: "linux", git_sha: null, host_metrics_json: null, registered_at_ms: now, last_seen_ms: now, reachable_addr: null, keeper_stale: null },
    { fp: workerB, dashboard_id: dashboardB, label: "B", os: "linux", git_sha: null, host_metrics_json: null, registered_at_ms: now, last_seen_ms: now, reachable_addr: null, keeper_stale: null },
  ]).execute();
  await db.insertInto("sessions").values([
    { id: sessionA, dashboard_id: dashboardA, worker_fp: workerA, channel: 1, kind: "shell", cwd: "/a", workspace_id: null, status: "open", agent_json: sql<undefined>`NULL`, created_at: now, closed_at: null, custom_title: null, git_branch: null, git_remote: null, pr_number: null, pr_state: null, pr_checks: null, pr_url: null, ports_json: null, spawn_cwd: null },
    { id: sessionB, dashboard_id: dashboardB, worker_fp: workerB, channel: 2, kind: "shell", cwd: "/b", workspace_id: null, status: "open", agent_json: sql<undefined>`NULL`, created_at: now, closed_at: null, custom_title: null, git_branch: null, git_remote: null, pr_number: null, pr_state: null, pr_checks: null, pr_url: null, ports_json: null, spawn_cwd: null },
  ]).execute();
  await db.insertInto("events").values([
    { dashboard_id: dashboardA, kind: "closed", session_id: sessionA, worker_fp: workerA, payload_json: JSON.stringify(closed(sessionA, 1)), ts: 1, client_seq: null },
    { dashboard_id: dashboardA, kind: "closed", session_id: sessionA, worker_fp: workerA, payload_json: JSON.stringify(closed(sessionA, 3)), ts: 3, client_seq: null },
    { dashboard_id: dashboardB, kind: "closed", session_id: sessionB, worker_fp: workerB, payload_json: JSON.stringify(closed(sessionB, 2)), ts: 2, client_seq: null },
  ]).execute();
});

afterAll(async () => {
  __setConnectWorkerForTest(workerB, null);
  await closeDb();
  if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
});

test("dashboard A feed excludes dashboard B durable, live, and retained frames", async () => {
  const scope: SyncDashboardScope = {
    dashboardId: dashboardA,
    workerFps: new Set([workerA]),
    sessionIds: new Set([sessionA]),
    workspaceIds: new Set(),
  };
  const frames: FirehoseFrame[] = [];
  const feed = startSyncFeed({ db } as ConnectDeps, scope, 1, (frame) => frames.push(frame), null);
  try {
    await feed.backfill();
    titleBus.publish({ session_id: sessionA, title: "A" });
    titleBus.publish({ session_id: sessionB, title: "B" });
    sessionBus.publish(Object.assign(closed(sessionA, 3), { _dashboard_id: dashboardA }));
    sessionBus.publish(Object.assign(closed(sessionB, 4), { _dashboard_id: dashboardB }));
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
    expect(titles[0]?.frame.case === "terminalTitle" && titles[0].frame.value.sessionId).toBe(sessionA);
  } finally {
    feed.dispose();
  }
});

test("foreign terminal view and input stop before cache or worker dispatch", async () => {
  let streamRequests = 0;
  const hub = new TerminalViewHub({
    db,
    resolveRoute: async (_dashboardId, _sessionId) => ({ workerFp: workerB, channel: 2, dashboardId: dashboardB }),
    sendStreamState: () => {
      streamRequests += 1;
      return { admitted: false, expired: false, requestId: null, result: Promise.reject(new Error("unexpected")) };
    },
    sendSnapshot: () => false,
  });
  const sink = {
    beginTerminalStream: () => true,
    enqueueTerminalState: () => {},
    replaceTerminalSnapshot: () => {},
    enqueueTerminalDelta: (): "queued" => "queued",
    dropTerminalSession: () => {},
  };
  let workerSends = 0;
  __setConnectWorkerForTest(workerB, { workerFp: workerB, dashboardId: dashboardB, send: () => { workerSends += 1; return 1; } });
  try {
    hub.registerSocket({
      socketId: "a", viewerKey: "a:tab", callerFingerprint: "a",
      dashboardId: dashboardA, allowsSession: (id) => id === sessionA, sink,
    });
    hub.handleViewCommand("a", create(TerminalViewCommandSchema, {
      viewId: "30000000-0000-4000-8000-000000000001", sessionId: sessionB,
      cols: 80, rows: 24, revision: 1n, active: true,
    }));
    await Promise.resolve();
    expect(streamRequests).toBe(0);
    const result = await processInputControl({ db } as ConnectDeps, {
      identity: { viewerKey: "a:tab", callerFingerprint: "a", dashboardId: dashboardA },
      sessionId: sessionB,
      inputSeq: 1n,
      data: new TextEncoder().encode("id\n"),
    });
    expect(result.status).toBe("rejected");
    expect(workerSends).toBe(0);
  } finally {
    hub.dispose();
  }
});
