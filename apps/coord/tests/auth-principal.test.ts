// Principal resolution: a verified key becomes exactly one persisted authority.
// Guards the admission boundary between JWT verification and RPC context, where
// a deleted, dual-role, or inactive identity must resolve to nothing at all.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";
import { resolveCallerPrincipal } from "../src/connect/auth-principal.ts";

const fpDevice = "fp-account-device";
const fpDisabled = "fp-disabled-account";
const fpWorker = "fp-worker";
const fpTombstonedWorker = "fp-worker-tombstoned";
const fpDualRole = "fp-dual-role";
const fpLegacy = "fp-legacy";
const disabledAccountId = "account-disabled";

let workdir: string;
let closeDb: () => Promise<void>;
let db: KyselyDB;
let activeAccountId: string;

function verified(fingerprint: string, label: string) {
  return { fingerprint, label };
}

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-auth-principal-"));
  const opened = openDb(join(workdir, "test.db"));
  db = opened.db;
  closeDb = opened.close;
  await runMigrations(opened.sqlite);
  const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  activeAccountId = tenant.accountId;

  const now = Date.now();
  await db.insertInto("authorized_keys").values([
    { fingerprint: fpDevice, public_key: new Uint8Array(32), label: "device", added_at: now },
    { fingerprint: fpDisabled, public_key: new Uint8Array(32), label: "disabled", added_at: now },
    { fingerprint: fpWorker, public_key: new Uint8Array(32), label: "worker", added_at: now },
    {
      fingerprint: fpTombstonedWorker,
      public_key: new Uint8Array(32),
      label: "tombstoned worker",
      added_at: now,
    },
    { fingerprint: fpDualRole, public_key: new Uint8Array(32), label: "dual role", added_at: now },
    { fingerprint: fpLegacy, public_key: new Uint8Array(32), label: "legacy", added_at: now },
  ]).execute();
  await db.insertInto("accounts").values({
    id: disabledAccountId,
    email_normalized: "disabled@example.test",
    status: "disabled",
    created_at_ms: now,
  }).execute();
  await db.insertInto("account_devices").values([
    { fingerprint: fpDevice, account_id: activeAccountId, added_at_ms: now, last_seen_at_ms: now },
    { fingerprint: fpDisabled, account_id: disabledAccountId, added_at_ms: now, last_seen_at_ms: now },
    { fingerprint: fpDualRole, account_id: activeAccountId, added_at_ms: now, last_seen_at_ms: now },
  ]).execute();
  await db.insertInto("workers").values([
    {
      fp: fpWorker,
      dashboard_id: tenant.dashboardId,
      label: "worker",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: now,
      last_seen_ms: now,
      reachable_addr: null,
    },
    {
      fp: fpTombstonedWorker,
      dashboard_id: tenant.dashboardId,
      label: "tombstoned worker",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: now,
      last_seen_ms: now,
      deleted_at_ms: now,
      reachable_addr: null,
    },
    {
      fp: fpDualRole,
      dashboard_id: tenant.dashboardId,
      label: "dual role",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: now,
      last_seen_ms: now,
      reachable_addr: null,
    },
  ]).execute();
});

afterAll(async () => {
  await closeDb?.();
  rmSync(workdir, { recursive: true, force: true });
});

test("a device key on an active account resolves browser authority", async () => {
  await expect(resolveCallerPrincipal(db, verified(fpDevice, "device"))).resolves.toEqual({
    kind: "account-device",
    fingerprint: fpDevice,
    label: "device",
    accountId: activeAccountId,
  });
});

test("a worker key resolves worker authority carrying no scope", async () => {
  await expect(resolveCallerPrincipal(db, verified(fpWorker, "worker"))).resolves.toEqual({
    kind: "worker",
    fingerprint: fpWorker,
    label: "worker",
  });
});

test("an authorized key with no device or worker row stays browser-capable", async () => {
  await expect(resolveCallerPrincipal(db, verified(fpLegacy, "legacy"))).resolves.toEqual({
    kind: "legacy-self-hosted",
    fingerprint: fpLegacy,
    label: "legacy",
  });
});

test("disabled account, tombstoned worker, dual authority, and unknown key all resolve to nothing", async () => {
  const [disabled, tombstoned, dualRole, unknown] = await Promise.all([
    resolveCallerPrincipal(db, verified(fpDisabled, "disabled")),
    resolveCallerPrincipal(db, verified(fpTombstonedWorker, "tombstoned worker")),
    resolveCallerPrincipal(db, verified(fpDualRole, "dual role")),
    resolveCallerPrincipal(db, verified("fp-never-authorized", "unknown")),
  ]);
  // Every rejected identity is intentionally indistinguishable at this boundary.
  expect([disabled, tombstoned, dualRole, unknown]).toEqual([null, null, null, null]);
});
