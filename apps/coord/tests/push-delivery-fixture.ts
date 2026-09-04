/**
 * Owns the coordinator, database, identity, and terminal-view fixtures shared by push-delivery tests.
 * Push-delivery suites create one fixture per file and reset mutable delivery state before each case.
 * It depends on real migrations, authentication, coordinator routing, and the terminal-view hub.
 */
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { CoordConfig } from "@roost/shared/config";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { X_ROOST_DASHBOARD_ID } from "@roost/shared/wire/headers";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { createCoord } from "../src/coord-factory.ts";
import { PasswordWorkGate } from "../src/connect/password-work-gate.ts";
import {
  installTerminalViewHub,
  TerminalViewHub,
} from "../src/connect/terminal-view-hub.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { newJwtCache, signJwt } from "../src/jwt.ts";
import { resetVapidKeysForTest } from "../src/vapid.ts";

export const SESSION_ID = "22222222-2222-4222-8222-222222222222";
export const VIEW_ID = "33333333-3333-4333-8333-333333333333";
export const VIEW_SOCKET_ID = "push-delivery-view-socket";
export const DASHBOARD_ID = "push-delivery-dashboard";
export const ACCOUNT_ID = "push-delivery-account";
export const PUSH_ORIGINS = ["https://push.example"] as const;

export interface PushDeliveryFixture {
  db: KyselyDB;
  cfg: CoordConfig;
  terminalViews: TerminalViewHub;
  viewerFp: string;
  rpc(method: string, body: object, authenticated?: boolean): Promise<Response>;
  reset(): Promise<void>;
  close(): Promise<void>;
}

export async function createPushDeliveryFixture(): Promise<PushDeliveryFixture> {
  const workdir = mkdtempSync(join(tmpdir(), "roost-push-"));
  const dbPath = join(workdir, "coord.db");
  const keyPath = join(workdir, "coord.key");
  const authorizedKeysPath = join(workdir, "authorized_keys");
  writeFileSync(authorizedKeysPath, "");
  const opened = openDb(dbPath);
  const { db, sqlite } = opened;
  await runMigrations(sqlite);
  const coordKey = await loadOrCreateCoordKey(keyPath);
  const jwtCache = newJwtCache();
  const cfg: CoordConfig = {
    trustProxy: false,
    saasMode: false,
    managedContainer: false,
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
    pushAllowedOrigins: [...PUSH_ORIGINS],
    logDir: workdir,
    publicUrl: undefined,
    handoffPath: join(workdir, "coord-handoff.json"),
  };

  const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const rawPublicKey = new Uint8Array(await crypto.subtle.exportKey("raw", keys.publicKey));
  const viewerFp = await fingerprintOf(rawPublicKey);
  await db.insertInto("authorized_keys").values({
    fingerprint: viewerFp,
    public_key: rawPublicKey,
    label: "push-test",
    added_at: Date.now(),
  }).execute();
  const fixtureNow = Date.now();
  await db.insertInto("accounts").values({
    id: ACCOUNT_ID,
    email_normalized: "push@example.com",
    password_hash: null,
    status: "active",
    created_at_ms: fixtureNow,
    password_changed_at_ms: null,
  }).execute();
  await db.insertInto("account_devices").values({
    fingerprint: viewerFp,
    account_id: ACCOUNT_ID,
    added_at_ms: fixtureNow,
    last_seen_at_ms: fixtureNow,
  }).execute();
  await db.insertInto("organizations").values({
    id: "push-delivery-organization",
    slug: "push-delivery",
    name: "Push Delivery",
    status: "active",
    created_at_ms: fixtureNow,
  }).execute();
  await db.insertInto("organization_memberships").values({
    organization_id: "push-delivery-organization",
    account_id: ACCOUNT_ID,
    role: "owner",
    created_at_ms: fixtureNow,
  }).execute();
  await db.insertInto("dashboards").values({
    id: DASHBOARD_ID,
    organization_id: "push-delivery-organization",
    slug: "push-delivery",
    name: "Push Delivery",
    status: "active",
    created_at_ms: fixtureNow,
  }).execute();
  await db.insertInto("dashboard_memberships").values({
    dashboard_id: DASHBOARD_ID,
    account_id: ACCOUNT_ID,
    role: "admin",
    created_at_ms: fixtureNow,
  }).execute();
  const now = Math.floor(Date.now() / 1_000);
  const jwt = await signJwt(
    { aud: "roost-coordinator", sub: viewerFp, iat: now, exp: now + 60 },
    keys.privateKey,
    viewerFp,
  );
  const terminalViews = new TerminalViewHub({
    db,
    resolveRoute: async () => null,
  });
  installTerminalViewHub(terminalViews);
  const coord = createCoord({
    db,
    sqlite,
    coordKey,
    cfg,
    jwtCache,
    passwordWorkGate: new PasswordWorkGate(),
  });

  return {
    db,
    cfg,
    terminalViews,
    viewerFp,
    rpc(method, body, authenticated = true) {
      const headers: Record<string, string> = { "content-type": "application/json" };
      headers[X_ROOST_DASHBOARD_ID] = DASHBOARD_ID;
      if (authenticated) headers.authorization = `Bearer ${jwt}`;
      return coord.fetch(new Request(`http://test/roost.v1.CoordinatorService/${method}`, {
        method: "POST",
        headers,
        body: JSON.stringify(body),
      }));
    },
    async reset() {
      cfg.pushAllowedOrigins = [...PUSH_ORIGINS];
      terminalViews.closeSession(SESSION_ID);
      await db.deleteFrom("push_subscriptions").execute();
      await db.deleteFrom("sessions").where("id", "=", SESSION_ID).execute();
    },
    async close() {
      installTerminalViewHub(null);
      terminalViews.dispose();
      coord.dispose();
      resetVapidKeysForTest();
      // finally: a close that throws (a leaked statement holding the file open)
      // must still leave the temp dir removed.
      try {
        await opened.close();
      } finally {
        if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
      }
    },
  };
}
