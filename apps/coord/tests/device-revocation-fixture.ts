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
  dashboardActorKey,
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
  dashboardRevocations: Array<{ dashboardId: string; fingerprint?: string }>;
  close(): Promise<void>;
  workerFences: string[];
  workerSyncRemovals: Array<{ dashboardId: string; fingerprint: string }>;
}

export interface DeviceRevocationHarnessOwner {
  cleanupHarnesses(): Promise<void>;
  openHarness(saasMode?: boolean): Promise<DeviceRevocationHarness>;
}

export function createDeviceRevocationHarnessOwner(): DeviceRevocationHarnessOwner {
  const cleanups: Array<() => Promise<void>> = [];

  async function cleanupHarnesses(): Promise<void> {
    for (const cleanup of cleanups.splice(0)) await cleanup();
  }

  async function openHarness(saasMode = false): Promise<DeviceRevocationHarness> {
    const dir = mkdtempSync(join(tmpdir(), "roost-device-revoke-"));
    const opened = openDb(join(dir, "test.db"));
    const { db, sqlite } = opened;
    await runMigrations(sqlite);
    const tenant = ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false });
    const revoked: string[] = [];
    const callbackStates: Array<{ keys: number; devices: number; pushes: number }> = [];
    const dashboardRevocations: Array<{ dashboardId: string; fingerprint?: string }> = [];
    const workerFences: string[] = [];
    const workerSyncRemovals: Array<{ dashboardId: string; fingerprint: string }> = [];
    const deps = {
      db,
      sqlite,
      cfg: { saasMode },
      jwtCache: newJwtCache(),
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
      onWorkerDeletedSyncScope: (dashboardId: string, fingerprint: string) => {
        workerSyncRemovals.push({ dashboardId, fingerprint });
        throw new Error("injected worker Sync cleanup failure");
      },
      onDashboardRevoked: (dashboardId: string, fingerprint?: string) =>
        dashboardRevocations.push({ dashboardId, fingerprint }),
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
      dashboardRevocations,
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

export function accountCtx(fingerprint: string, accountId: string): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint,
    label: "test",
    accountId,
  });
  return { values } as unknown as HandlerContext;
}

export function workerCtx(fingerprint: string, dashboardId: string): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "worker",
    fingerprint,
    label: "worker",
    dashboardId,
  });
  return { values } as unknown as HandlerContext;
}

export function unauthCtx(address: string, onHost: boolean): HandlerContext {
  const values = createContextValues();
  values.set(remoteAddressKey, address);
  values.set(onHostKey, onHost);
  return { values } as unknown as HandlerContext;
}

export const workerDeleteDashboardId = "worker-delete-dashboard";

export function dashboardAdminCtx(): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: "administrator",
    label: "administrator",
    accountId: "administrator-account",
  });
  values.set(dashboardActorKey, {
    accountId: "administrator-account",
    organizationId: "worker-delete-organization",
    dashboardId: workerDeleteDashboardId,
    organizationRole: "owner",
    dashboardRole: "admin",
    deviceFingerprint: "administrator",
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

export async function addAccountDevice(
  db: KyselyDB,
  accountId: string,
  device: DeviceKey,
  label: string,
): Promise<void> {
  const now = Date.now();
  await db.insertInto("accounts").values({
    id: accountId,
    email_normalized: `${accountId}@example.test`,
    password_hash: null,
    status: "active",
    created_at_ms: now,
    password_changed_at_ms: null,
  }).execute();
  await authorize(db, device, label);
  await db.insertInto("account_devices").values({
    fingerprint: device.fingerprint,
    account_id: accountId,
    added_at_ms: now,
    last_seen_at_ms: now,
  }).execute();
}

export async function token(
  h: DeviceRevocationHarness,
  value: string,
  minter: string | null,
  kind: "browser" | "worker" = "browser",
  dashboardId = h.tenant.dashboardId,
): Promise<void> {
  const membership = await h.db.selectFrom("dashboard_memberships")
    .select("account_id")
    .where("dashboard_id", "=", dashboardId)
    .executeTakeFirstOrThrow();
  const now = Date.now();
  await h.db.insertInto("bootstrap_tokens").values({
    token_hash: await bootstrapTokenDigest(value),
    account_id: membership.account_id,
    dashboard_id: dashboardId,
    kind,
    label: "new browser",
    created_at_ms: now,
    expires_at_ms: now + 60_000,
    used_at_ms: null,
    used_by_fp: null,
    minted_by_fp: minter,
  }).execute();
}
