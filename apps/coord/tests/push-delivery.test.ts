/**
 * Covers Web Push configuration and subscription RPC behavior.
 * The suite uses the shared push-delivery fixture and calls the real coordinator routes.
 * Sender transport and active-view suppression behavior live in the sibling sender suite.
 */
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import type { CoordConfig } from "@roost/shared/config";
import type { KyselyDB } from "../src/db/connection.ts";
import { getVapidKeys, resetVapidKeysForTest } from "../src/vapid.ts";
import {
  createPushDeliveryFixture,
  type PushDeliveryFixture,
} from "./push-delivery-fixture.ts";

let fixture: PushDeliveryFixture;
let db: KyselyDB;
let cfg: CoordConfig;
let viewerFp: string;
let rpc: PushDeliveryFixture["rpc"];

beforeAll(async () => {
  fixture = await createPushDeliveryFixture();
  ({ db, cfg, viewerFp, rpc } = fixture);
});

beforeEach(async () => {
  await fixture.reset();
});

afterAll(async () => {
  await fixture.close();
});

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

  test("stays disabled without creating VAPID state when no provider origin is configured", async () => {
    await db.deleteFrom("app_settings").where("key", "=", "push.vapid").execute();
    resetVapidKeysForTest();
    cfg.pushAllowedOrigins = [];

    const configResponse = await rpc("PushGetConfig", {});
    expect(configResponse.status).toBe(200);
    const body = await configResponse.json() as {
      available?: boolean;
      vapidPublicKeyB64?: string;
    };
    expect(body.available ?? false).toBe(false);
    expect(body.vapidPublicKeyB64 ?? "").toBe("");
    expect(await db.selectFrom("app_settings").selectAll()
      .where("key", "=", "push.vapid").execute()).toHaveLength(0);

    const subscription = {
      endpoint: "https://push.example/disabled",
      p256dh: "abc",
      auth: "def",
    };
    expect((await rpc("PushSubscribe", subscription)).status).not.toBe(200);
    expect((await rpc("PushUnsubscribe", { endpoint: subscription.endpoint })).status).not.toBe(200);
    expect(await db.selectFrom("push_subscriptions").selectAll().execute()).toHaveLength(0);
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

    for (const endpoint of [
      "https://push.example.attacker.invalid/subscription",
      "https://127.0.0.1/subscription",
      "https://push.example:8443/subscription",
      "https://user@push.example/subscription",
      "https://push.example/subscription#fragment",
    ]) {
      expect((await rpc("PushSubscribe", {
        endpoint,
        p256dh: "abc",
        auth: "def",
      })).status, endpoint).toBe(400);
    }

    const endpoint = "https://push.example/subscription-token";
    expect((await rpc("PushSubscribe", { endpoint, p256dh: "abc", auth: "def" })).status).toBe(200);
    expect((await rpc("PushSubscribe", { endpoint, p256dh: "updated", auth: "updated" })).status).toBe(200);
    const rows = await db.selectFrom("push_subscriptions").selectAll().execute();
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ viewer_fp: viewerFp, endpoint, p256dh: "updated", auth: "updated" });
    expect((await rpc("PushUnsubscribe", { endpoint })).status).toBe(200);
    expect(await db.selectFrom("push_subscriptions").selectAll().execute()).toHaveLength(0);
  });

  test("atomically caps each account device at four distinct subscriptions", async () => {
    for (let index = 0; index < 4; index++) {
      expect((await rpc("PushSubscribe", {
        endpoint: `https://push.example/subscription-${index}`,
        p256dh: `key${index}`,
        auth: `auth${index}`,
      })).status).toBe(200);
    }
    expect((await rpc("PushSubscribe", {
      endpoint: "https://push.example/subscription-4",
      p256dh: "key4",
      auth: "auth4",
    })).status).toBe(429);

    expect((await rpc("PushSubscribe", {
      endpoint: "https://push.example/subscription-0",
      p256dh: "updated",
      auth: "updated",
    })).status).toBe(200);
    const rows = await db.selectFrom("push_subscriptions")
      .select(["endpoint", "p256dh", "auth"])
      .where("viewer_fp", "=", viewerFp)
      .orderBy("endpoint")
      .execute();
    expect(rows).toHaveLength(4);
    expect(rows[0]).toEqual({
      endpoint: "https://push.example/subscription-0",
      p256dh: "updated",
      auth: "updated",
    });
  });

});
