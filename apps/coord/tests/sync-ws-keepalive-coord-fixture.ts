/**
 * Owns the database and identity fixture shared by Sync WebSocket keepalive suites.
 * Each discovered suite creates and closes its own instance so mutations cannot leak across files.
 * It depends on real coordinator migrations, key generation, JWT signing, and membership tables.
 */
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { CoordConfig } from "@roost/shared/config";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { PasswordWorkGate } from "../src/connect/password-work-gate.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { newJwtCache, signJwt } from "../src/jwt.ts";

const ACCOUNT_ID = "sync-keepalive-account";
const ORGANIZATION_ID = "sync-keepalive-org";
export const SYNC_WS_KEEPALIVE_DASHBOARD_ID = "sync-keepalive-dashboard";

export interface SyncWsKeepaliveCoordFixture {
  deps: ConnectDeps;
  fingerprint: string;
  jwt: string;
  close(): Promise<void>;
}

export async function createSyncWsKeepaliveCoordFixture(): Promise<SyncWsKeepaliveCoordFixture> {
  const workdir = mkdtempSync(join(tmpdir(), "roost-sync-keepalive-"));
  const dbPath = join(workdir, "test.db");
  const keyPath = join(workdir, "test.key");
  const authorizedKeysPath = join(workdir, "authorized_keys");
  writeFileSync(authorizedKeysPath, "");

  const opened = openDb(dbPath);
  const { db, sqlite } = opened;
  await runMigrations(sqlite);
  const coordKey = await loadOrCreateCoordKey(keyPath);
  const jwtCache = newJwtCache();
  const cfg: CoordConfig = {
    saasMode: false,
    managedContainer: false,
    pushAllowedOrigins: [],
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
    trustProxy: false,
    corsAllowedOrigins: [],
    logDir: workdir,
    webPublicUrl: "https://public.example",
    publicUrl: undefined,
    handoffPath: join(workdir, "coord-handoff.json"),
  };
  const deps: ConnectDeps = {
    db,
    sqlite,
    coordKey,
    jwtCache,
    cfg,
    passwordWorkGate: new PasswordWorkGate(),
  };

  const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const rawPublicKey = new Uint8Array(await crypto.subtle.exportKey("raw", keys.publicKey));
  const fingerprint = await fingerprintOf(rawPublicKey);
  await db.insertInto("authorized_keys").values({
    fingerprint,
    public_key: rawPublicKey,
    label: "test-web",
    added_at: Date.now(),
  }).execute();
  const membershipNow = Date.now();
  await db.insertInto("accounts").values({
    id: ACCOUNT_ID,
    email_normalized: "sync-keepalive@example.test",
    password_hash: null,
    status: "active",
    created_at_ms: membershipNow,
    password_changed_at_ms: null,
  }).execute();
  await db.insertInto("account_devices").values({
    fingerprint,
    account_id: ACCOUNT_ID,
    added_at_ms: membershipNow,
    last_seen_at_ms: membershipNow,
  }).execute();
  await db.insertInto("organizations").values({
    id: ORGANIZATION_ID,
    slug: "sync-keepalive-org",
    name: "Sync keepalive",
    status: "active",
    created_at_ms: membershipNow,
  }).execute();
  await db.insertInto("organization_memberships").values({
    organization_id: ORGANIZATION_ID,
    account_id: ACCOUNT_ID,
    role: "owner",
    created_at_ms: membershipNow,
  }).execute();
  await db.insertInto("dashboards").values({
    id: SYNC_WS_KEEPALIVE_DASHBOARD_ID,
    organization_id: ORGANIZATION_ID,
    slug: "sync-keepalive",
    name: "Sync keepalive",
    status: "active",
    created_at_ms: membershipNow,
  }).execute();
  await db.insertInto("dashboard_memberships").values({
    dashboard_id: SYNC_WS_KEEPALIVE_DASHBOARD_ID,
    account_id: ACCOUNT_ID,
    role: "admin",
    created_at_ms: membershipNow,
  }).execute();
  const now = Math.floor(Date.now() / 1000);
  const jwt = await signJwt(
    { aud: "roost-coordinator", sub: fingerprint, iat: now, exp: now + 60 },
    keys.privateKey,
    fingerprint,
  );

  return {
    deps,
    fingerprint,
    jwt,
    async close() {
      try {
        await opened.close();
      } finally {
        if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
      }
    },
  };
}
