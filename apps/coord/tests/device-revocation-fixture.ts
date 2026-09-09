/**
 * Owns shared setup for the split device-revocation test suites.
 * Sibling suites call it to create isolated databases, principals, and grants.
 * It depends on coordinator handlers, migrations, and authentication context keys.
 */
import {
  createContextValues,
  type HandlerContext,
  type ServiceImpl,
} from "@connectrpc/connect";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import type { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { bootstrapTokenDigest } from "../src/bootstrap-tokens.ts";
import {
  callerKey,
  onHostKey,
  remoteAddressKey,
} from "../src/connect/auth-interceptor.ts";
import { makeAuthHandlers } from "../src/connect/handlers-auth.ts";
import { makeWorkerHandlers } from "../src/connect/handlers-workers.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { newJwtCache } from "../src/jwt.ts";
import {
  ensureSelfHostedTenant,
  type SelfHostedTenant,
} from "../src/self-hosted-tenant.ts";

type AuthHandlers = Pick<
  ServiceImpl<typeof CoordinatorService>,
  | "authLogout"
  | "authRedeemBrowser"
  | "authRedeemWorker"
  | "devicesList"
  | "devicesRevoke"
  | "devicesRotateCurrent"
  | "pairCreate"
>;

type WorkerHandlers = Pick<ServiceImpl<typeof CoordinatorService>, "workersDelete">;

export interface DeviceKey {
  raw: Uint8Array;
  b64: string;
  fingerprint: string;
}

export interface DeviceRevocationHarness {
  db: KyselyDB;
  sqlite: Database;
  tenant: SelfHostedTenant;
  handlers: AuthHandlers;
  workerHandlers: WorkerHandlers;
  revoked: string[];
  callbackStates: Array<{ keys: number; devices: number; pushes: number }>;
  close(): Promise<void>;
  workerFences: string[];
  workerSyncRemovals: string[];
}

export interface DeviceRevocationHarnessOwner {
  cleanupHarnesses(): Promise<void>;
  openHarness(): Promise<DeviceRevocationHarness>;
}

export function createDeviceRevocationHarnessOwner(): DeviceRevocationHarnessOwner {
  const cleanups: Array<() => Promise<void>> = [];

  async function cleanupHarnesses(): Promise<void> {
    for (const cleanup of cleanups.splice(0)) await cleanup();
  }

  async function openHarness(): Promise<DeviceRevocationHarness> {
    const dir = mkdtempSync(join(tmpdir(), "roost-device-revoke-"));
    const opened = openDb(join(dir, "test.db"));
    const { db, sqlite } = opened;
    await runMigrations(sqlite);
    const tenant = ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false });
    const revoked: string[] = [];
    const callbackStates: Array<{ keys: number; devices: number; pushes: number }> = [];
    const workerFences: string[] = [];
    const workerSyncRemovals: string[] = [];
    const deps = {
      db,
      sqlite,
      cfg: {},
      jwtCache: newJwtCache(),
      selfHostedTenant: tenant,
      onKeyRevoked: (fingerprint: string) => {
        revoked.push(fingerprint);
        const keyCount = sqlite.query("SELECT COUNT(*) AS count FROM authorized_keys WHERE fingerprint = ?")
          .get(fingerprint) as { count: number };
        const deviceCount = sqlite.query("SELECT COUNT(*) AS count FROM account_devices WHERE fingerprint = ?")
          .get(fingerprint) as { count: number };
        const pushCount = sqlite.query("SELECT COUNT(*) AS count FROM push_subscriptions WHERE viewer_fp = ?")
          .get(fingerprint) as { count: number };
        callbackStates.push({
          keys: Number(keyCount.count),
          devices: Number(deviceCount.count),
          pushes: Number(pushCount.count),
        });
      },
      onWorkerDeletedSocketClose: (fingerprint: string) => {
        revoked.push(fingerprint);
      },
      onWorkerDeletedFence: (fingerprint: string) => {
        workerFences.push(fingerprint);
      },
      onWorkerDeletedSyncScope: (fingerprint: string) => {
        workerSyncRemovals.push(fingerprint);
        throw new Error("injected worker Sync cleanup failure");
      },
    } as unknown as ConnectDeps;
    const close = async () => {
      try { await opened.close(); } finally { rmSync(dir, { recursive: true, force: true }); }
    };
    cleanups.push(close);
    return {
      db,
      sqlite,
      tenant,
      handlers: makeAuthHandlers(deps),
      workerHandlers: makeWorkerHandlers(deps),
      revoked,
      callbackStates,
      workerFences,
      workerSyncRemovals,
      close,
    };
  }

  return { cleanupHarnesses, openHarness };
}

export function authCtx(fingerprint: string): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, { kind: "legacy-self-hosted", fingerprint, label: "test" });
  return { values } as unknown as HandlerContext;
}

export function unauthCtx(address: string, onHost: boolean): HandlerContext {
  const values = createContextValues();
  values.set(remoteAddressKey, address);
  values.set(onHostKey, onHost);
  return { values } as unknown as HandlerContext;
}

export function browserDeviceCtx(accountId: string): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: "administrator",
    label: "administrator",
    accountId,
  });
  return { values } as unknown as HandlerContext;
}

export async function key(): Promise<DeviceKey> {
  const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const raw = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  return {
    raw,
    b64: Buffer.from(raw).toString("base64"),
    fingerprint: await fingerprintOf(raw),
  };
}

export async function authorize(db: KyselyDB, device: DeviceKey, label: string): Promise<void> {
  await db.insertInto("authorized_keys").values({
    fingerprint: device.fingerprint,
    public_key: device.raw,
    label,
    added_at: Date.now(),
  }).execute();
}

export async function token(
  h: DeviceRevocationHarness,
  value: string,
  minter: string | null,
  kind: "browser" | "worker" = "browser",
): Promise<void> {
  const now = Date.now();
  await h.db.insertInto("bootstrap_tokens").values({
    token_hash: await bootstrapTokenDigest(value),
    account_id: h.tenant.accountId,
    dashboard_id: h.tenant.dashboardId,
    kind,
    label: "new browser",
    created_at_ms: now,
    expires_at_ms: now + 60_000,
    used_at_ms: null,
    used_by_fp: null,
    minted_by_fp: minter,
  }).execute();
}
