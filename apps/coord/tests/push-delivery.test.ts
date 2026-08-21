import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Database } from "bun:sqlite";
import { create } from "@bufbuild/protobuf";
import { TerminalViewCommandSchema } from "@roost/shared/proto/sync_pb";
import type { CoordConfig } from "@roost/shared/config";
import { openDb, type DbHandle, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { newJwtCache, signJwt } from "../src/jwt.ts";
import { createCoord, type CoordHandle } from "../src/coord-factory.ts";
import { getVapidKeys, resetVapidKeysForTest } from "../src/vapid.ts";
import {
  sendPushToSubscriptions,
  type PushNotificationTransport,
} from "../src/push-sender.ts";
import { firePushForTransition } from "../src/push-dispatch.ts";
import {
  installTerminalViewHub,
  TerminalViewHub,
} from "../src/connect/terminal-view-hub.ts";

const SESSION_ID = "22222222-2222-4222-8222-222222222222";
const VIEW_ID = "33333333-3333-4333-8333-333333333333";
const VIEW_SOCKET_ID = "push-delivery-view-socket";
let workdir: string;
let db: KyselyDB;
let sqlite: Database;
let opened: DbHandle;
let coord: CoordHandle;
let terminalViews: TerminalViewHub;
let jwt: string;
let viewerFp: string;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-push-"));
  const dbPath = join(workdir, "coord.db");
  const keyPath = join(workdir, "coord.key");
  const authorizedKeysPath = join(workdir, "authorized_keys");
  writeFileSync(authorizedKeysPath, "");
  opened = openDb(dbPath);
  db = opened.db;
  sqlite = opened.sqlite;
  await runMigrations(sqlite);
  const coordKey = await loadOrCreateCoordKey(keyPath);
  const jwtCache = newJwtCache();
  const cfg: CoordConfig = {
    trustProxy: false,
    bind: "127.0.0.1:0",
    dbPath,
    coordKeyPath: keyPath,
    authorizedKeysPath,
    webDistPath: "",
    tlsCertPath: undefined,
    tlsKeyPath: undefined,
    jwtMaxAgeSecs: 300,
    auditRetentionDays: 90,
    relaxedCsp: false,
    corsAllowedOrigins: [],
    logDir: workdir,
    publicUrl: undefined,
    handoffPath: join(workdir, "coord-handoff.json"),
  };

  const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const rawPublicKey = new Uint8Array(await crypto.subtle.exportKey("raw", keys.publicKey));
  viewerFp = await fingerprintOf(rawPublicKey);
  await db.insertInto("authorized_keys").values({
    fingerprint: viewerFp,
    public_key: rawPublicKey,
    label: "push-test",
    added_at: Date.now(),
  }).execute();
  const now = Math.floor(Date.now() / 1_000);
  jwt = await signJwt(
    { aud: "roost-coordinator", sub: viewerFp, iat: now, exp: now + 60 },
    keys.privateKey,
    viewerFp,
  );
  terminalViews = new TerminalViewHub({
    db,
    resolveRoute: async () => null,
  });
  installTerminalViewHub(terminalViews);
  coord = createCoord({ db, sqlite, coordKey, cfg, jwtCache });
});

beforeEach(async () => {
  terminalViews.closeSession(SESSION_ID);
  await db.deleteFrom("push_subscriptions").execute();
  await db.deleteFrom("sessions").where("id", "=", SESSION_ID).execute();
});

afterAll(async () => {
  installTerminalViewHub(null);
  terminalViews?.dispose();
  coord?.dispose();
  resetVapidKeysForTest();
  // finally: a close that throws (a leaked statement holding the file open)
  // must still leave the temp dir removed.
  try {
    await opened?.close();
  } finally {
    if (workdir && existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
  }
});

function rpc(method: string, body: object, authenticated = true): Promise<Response> {
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (authenticated) headers.authorization = `Bearer ${jwt}`;
  return coord.fetch(new Request(`http://test/roost.v1.CoordinatorService/${method}`, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  }));
}

describe("Web Push delivery", () => {
  test("serializes first-use VAPID generation and persists one identity", async () => {
    await db.deleteFrom("app_settings").where("key", "=", "push.vapid").execute();
    resetVapidKeysForTest();
    const keys = await Promise.all(Array.from({ length: 8 }, () => getVapidKeys(db)));
    expect(new Set(keys.map((value) => value.publicKey))).toHaveLength(1);
    expect(new Set(keys.map((value) => value.privateKey))).toHaveLength(1);
    const rows = await db.selectFrom("app_settings")
      .selectAll().where("key", "=", "push.vapid").execute();
    expect(rows).toHaveLength(1);
  });

  test("authenticates, validates, upserts, and removes subscriptions", async () => {
    expect((await rpc("PushGetConfig", {}, false)).status).toBe(401);
    const configResponse = await rpc("PushGetConfig", {});
    expect(configResponse.status).toBe(200);
    expect(await configResponse.json()).toMatchObject({ available: true });

    expect((await rpc("PushSubscribe", {
      endpoint: "http://push.example/not-secure",
      p256dh: "abc",
      auth: "def",
    })).status).toBe(400);

    const endpoint = "https://push.example/subscription-token";
    expect((await rpc("PushSubscribe", { endpoint, p256dh: "abc", auth: "def" })).status).toBe(200);
    expect((await rpc("PushSubscribe", { endpoint, p256dh: "updated", auth: "updated" })).status).toBe(200);
    const rows = await db.selectFrom("push_subscriptions").selectAll().execute();
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ viewer_fp: viewerFp, endpoint, p256dh: "updated", auth: "updated" });
    expect((await rpc("PushUnsubscribe", { endpoint })).status).toBe(200);
    expect(await db.selectFrom("push_subscriptions").selectAll().execute()).toHaveLength(0);
  });

  test("prunes 404 and 410 subscriptions without exposing endpoints", async () => {
    const subscription = {
      viewer_fp: viewerFp,
      endpoint: "https://push.example/expired-secret-token",
      p256dh: "abc",
      auth: "def",
      created_at_ms: Date.now(),
    };
    await db.insertInto("push_subscriptions").values(subscription).execute();
    const goneTransport: PushNotificationTransport = {
      sendNotification: async () => { throw { statusCode: 410 }; },
    };
    const result = await sendPushToSubscriptions(db, [subscription], { test: true }, goneTransport);
    expect(result).toEqual({ delivered: 0, expired: 1, failed: 0 });
    expect(await db.selectFrom("push_subscriptions").selectAll().execute()).toHaveLength(0);
  });

  test("suppresses only devices actively viewing the session", async () => {
    const workerFp = "cc".repeat(32);
    await db.insertInto("workers").values({
      fp: workerFp,
      label: "push-worker",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: Date.now(),
      last_seen_ms: Date.now(),
      reachable_addr: null,
      keeper_stale: null,
    }).onConflict((conflict) => conflict.column("fp").doNothing()).execute();
    await db.insertInto("sessions").values({
      id: SESSION_ID,
      worker_fp: workerFp,
      channel: 1,
      kind: "shell",
      cwd: "/work/project",
      workspace_id: null,
      status: "open",
      created_at: Date.now(),
      closed_at: null,
      custom_title: null,
      git_branch: null,
      git_remote: null,
      pr_number: null,
      pr_state: null,
      pr_checks: null,
      pr_url: null,
      ports_json: null,
      spawn_cwd: "/work/project",
    }).execute();
    const otherFp = "dd".repeat(32);
    await db.insertInto("push_subscriptions").values([
      { viewer_fp: viewerFp, endpoint: "https://push.example/viewing", p256dh: "a", auth: "b", created_at_ms: 1 },
      { viewer_fp: otherFp, endpoint: "https://push.example/background", p256dh: "c", auth: "d", created_at_ms: 1 },
    ]).execute();
    terminalViews.registerSocket({
      socketId: VIEW_SOCKET_ID,
      viewerKey: `${viewerFp}:push-delivery-tab`,
      callerFingerprint: viewerFp,
      sink: {
        beginTerminalStream() {},
        enqueueTerminalState() {},
        replaceTerminalSnapshot() {},
        enqueueTerminalDelta: () => true,
        dropTerminalSession() {},
      },
    });
    terminalViews.handleViewCommand(
      VIEW_SOCKET_ID,
      create(TerminalViewCommandSchema, {
        viewId: VIEW_ID,
        sessionId: SESSION_ID,
        revision: 1n,
        cols: 80,
        rows: 24,
        active: true,
      }),
    );

    const deliveries: Array<{ viewerFps: string[]; payload: object }> = [];
    const sender: typeof sendPushToSubscriptions = async (_db, subscriptions, payload) => {
      deliveries.push({ viewerFps: subscriptions.map((value) => value.viewer_fp), payload });
      return { delivered: subscriptions.length, expired: 0, failed: 0 };
    };
    await firePushForTransition(db, SESSION_ID, "blocked", sender);
    expect(deliveries).toEqual([{
      viewerFps: [otherFp],
      payload: {
        sessionId: SESSION_ID,
        kind: "blocked",
        title: "project",
        body: "Needs your input",
      },
    }]);

    await db.deleteFrom("push_subscriptions").where("viewer_fp", "=", otherFp).execute();
    await firePushForTransition(db, SESSION_ID, "done", sender);
    expect(deliveries).toHaveLength(1);
  });
});
