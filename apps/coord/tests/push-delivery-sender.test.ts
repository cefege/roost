/**
 * Covers Web Push transport delivery and dispatch suppression behavior.
 * The suite uses the shared push-delivery fixture and exercises real database filtering.
 * Configuration and subscription RPC behavior remain in the primary push-delivery suite.
 */
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { TerminalViewCommandSchema } from "@roost/shared/proto/sync_pb";
import type { KyselyDB } from "../src/db/connection.ts";
import { firePushForTransition } from "../src/push-dispatch.ts";
import {
  sendPushToSubscriptions,
  type PushNotificationTransport,
} from "../src/push-sender.ts";
import type { TerminalViewHub } from "../src/connect/terminal-view-hub.ts";
import {
  ACCOUNT_ID,
  createPushDeliveryFixture,
  DASHBOARD_ID,
  PUSH_ORIGINS,
  SESSION_ID,
  type PushDeliveryFixture,
  VIEW_ID,
  VIEW_SOCKET_ID,
} from "./push-delivery-fixture.ts";

let fixture: PushDeliveryFixture;
let db: KyselyDB;
let terminalViews: TerminalViewHub;
let viewerFp: string;

beforeAll(async () => {
  fixture = await createPushDeliveryFixture();
  ({ db, terminalViews, viewerFp } = fixture);
});

beforeEach(async () => {
  await fixture.reset();
});

afterAll(async () => {
  await fixture.close();
});

describe("Web Push delivery", () => {
  test("prunes 404 and 410 subscriptions without exposing endpoints", async () => {
    const subscription = {
      dashboard_id: DASHBOARD_ID,
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

  test("bounds sends to four, applies the timeout, and never retries redirects", async () => {
    const subscriptions = Array.from({ length: 9 }, (_, index) => ({
      dashboard_id: DASHBOARD_ID,
      viewer_fp: viewerFp,
      endpoint: `https://push.example/concurrency-${index}`,
      p256dh: "abc",
      auth: "def",
      created_at_ms: index,
    }));
    let inFlight = 0;
    let maxInFlight = 0;
    let releaseSends!: () => void;
    const sendsReleased = new Promise<void>((resolve) => {
      releaseSends = resolve;
    });
    let observeFourStarted!: () => void;
    const fourStarted = new Promise<void>((resolve) => {
      observeFourStarted = resolve;
    });
    const timeouts: Array<number | undefined> = [];
    const boundedTransport: PushNotificationTransport = {
      sendNotification: async (_subscription, _payload, options) => {
        inFlight++;
        maxInFlight = Math.max(maxInFlight, inFlight);
        timeouts.push(options?.timeout);
        if (inFlight === 4) observeFourStarted();
        await sendsReleased;
        inFlight--;
        return {} as never;
      },
    };
    const deliveries = sendPushToSubscriptions(
      db,
      subscriptions,
      { test: true },
      boundedTransport,
    );
    await fourStarted;
    expect(inFlight).toBe(4);
    releaseSends();
    expect(await deliveries).toEqual({ delivered: 9, expired: 0, failed: 0 });
    expect(maxInFlight).toBe(4);
    expect(timeouts).toEqual(Array(9).fill(10_000));

    let redirectCalls = 0;
    const redirectTransport: PushNotificationTransport = {
      sendNotification: async () => {
        redirectCalls++;
        throw { statusCode: 302 };
      },
    };
    expect(await sendPushToSubscriptions(
      db,
      [subscriptions[0]!],
      { test: true },
      redirectTransport,
    )).toEqual({ delivered: 0, expired: 0, failed: 1 });
    expect(redirectCalls).toBe(1);
  });

  test("suppresses only devices actively viewing the session", async () => {
    const workerFp = "cc".repeat(32);
    await db.insertInto("workers").values({
      dashboard_id: DASHBOARD_ID,
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
      dashboard_id: DASHBOARD_ID,
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
    await db.insertInto("authorized_keys").values({
      fingerprint: otherFp,
      public_key: new Uint8Array(32),
      label: "push-background",
      added_at: Date.now(),
    }).onConflict((conflict) => conflict.column("fingerprint").doNothing()).execute();
    await db.insertInto("account_devices").values({
      fingerprint: otherFp,
      account_id: ACCOUNT_ID,
      added_at_ms: Date.now(),
      last_seen_at_ms: Date.now(),
    }).execute();
    await db.insertInto("push_subscriptions").values([
      { dashboard_id: DASHBOARD_ID, viewer_fp: viewerFp, endpoint: "https://push.example/viewing", p256dh: "a", auth: "b", created_at_ms: 1 },
      { dashboard_id: DASHBOARD_ID, viewer_fp: otherFp, endpoint: "https://push.example/background", p256dh: "c", auth: "d", created_at_ms: 1 },
    ]).execute();
    terminalViews.registerSocket({
      socketId: VIEW_SOCKET_ID,
      viewerKey: `${viewerFp}:push-delivery-tab`,
      dashboardId: DASHBOARD_ID,
      allowsSession: (sessionId) => sessionId === SESSION_ID,
      callerFingerprint: viewerFp,
      sink: {
        beginTerminalStream: () => true,
        enqueueTerminalState() {},
        replaceTerminalSnapshot() {},
        enqueueTerminalDelta: () => "queued",
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
    await firePushForTransition(db, SESSION_ID, "blocked", PUSH_ORIGINS, sender, "a".repeat(64));
    expect(deliveries).toEqual([{
      viewerFps: [otherFp],
      payload: {
        sessionId: SESSION_ID,
        kind: "blocked",
        title: "project",
        body: "Needs your input",
        routeKey: "a".repeat(64),
      },
    }]);

    // Removing only the live account-device association leaves a legacy Push
    // row behind; dispatch must prune it and must not deliver to it.
    await db.deleteFrom("account_devices").where("fingerprint", "=", otherFp).execute();
    await firePushForTransition(db, SESSION_ID, "done", PUSH_ORIGINS, sender);
    expect(deliveries).toHaveLength(1);
    expect(await db.selectFrom("push_subscriptions").select("viewer_fp")
      .where("viewer_fp", "=", otherFp).execute()).toHaveLength(0);

    // Disabled accounts retain state for operator re-enable but receive no
    // notification while inactive.
    await db.updateTable("accounts").set({ status: "disabled" })
      .where("id", "=", ACCOUNT_ID).execute();
    try {
      terminalViews.closeSession(SESSION_ID);
      await firePushForTransition(db, SESSION_ID, "done", PUSH_ORIGINS, sender);
      expect(deliveries).toHaveLength(1);
    } finally {
      await db.updateTable("accounts").set({ status: "active" })
        .where("id", "=", ACCOUNT_ID).execute();
    }
  });
});
