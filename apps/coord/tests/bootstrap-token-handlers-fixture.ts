/**
 * Owns isolated databases, identities, and cleanup for bootstrap handler tests.
 * The prefixed bootstrap token handler suites call these fixtures for shared auth state.
 * It depends on coord migrations, auth handler wiring, and fingerprint generation.
 */
import { expect } from "bun:test";
import {
  Code,
  ConnectError,
  type ServiceImpl,
} from "@connectrpc/connect";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import type { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { makeAuthHandlers } from "../src/connect/handlers-auth.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { newJwtCache } from "../src/jwt.ts";
import { ensureSelfHostedTenant, type SelfHostedTenant } from "../src/self-hosted-tenant.ts";

type AuthHandlers = Pick<ServiceImpl<typeof CoordinatorService>,
  "authMintBootstrap" | "authRedeemBrowser" | "authRedeemWorker">;

export interface TestKey {
  raw: Uint8Array;
  b64: string;
  fingerprint: string;
}

export interface Harness {
  db: KyselyDB;
  sqlite: Database;
  tenant: SelfHostedTenant;
  deps: ConnectDeps;
  handlers: AuthHandlers;
}

export function createBootstrapHandlerHarnessOwner(): {
  cleanupHarnesses: () => Promise<void>;
  openHarness: (saasMode?: boolean) => Promise<Harness>;
} {
  const cleanups: Array<() => Promise<void>> = [];

  async function cleanupHarnesses(): Promise<void> {
    for (const cleanup of cleanups.splice(0)) await cleanup();
  }

  async function openHarness(saasMode = false): Promise<Harness> {
    const dir = mkdtempSync(join(tmpdir(), "roost-bootstrap-handler-"));
    const opened = openDb(join(dir, "coord.db"));
    await runMigrations(opened.sqlite);
    const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
    const deps = {
      db: opened.db,
      sqlite: opened.sqlite,
      cfg: { saasMode },
      jwtCache: newJwtCache(),
    } as unknown as ConnectDeps;
    cleanups.push(async () => {
      try {
        await opened.close();
      } finally {
        rmSync(dir, { recursive: true, force: true });
      }
    });
    return {
      db: opened.db,
      sqlite: opened.sqlite,
      tenant,
      deps,
      handlers: makeAuthHandlers(deps),
    };
  }

  return { cleanupHarnesses, openHarness };
}

export async function makeKey(): Promise<TestKey> {
  const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const raw = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  return {
    raw,
    b64: Buffer.from(raw).toString("base64"),
    fingerprint: await fingerprintOf(raw),
  };
}

export async function authorizeAccountDevice(h: Harness, key: TestKey): Promise<void> {
  const now = Date.now();
  await h.db.insertInto("authorized_keys").values({
    fingerprint: key.fingerprint,
    public_key: key.raw,
    label: "actor",
    added_at: now,
  }).execute();
  await h.db.insertInto("account_devices").values({
    fingerprint: key.fingerprint,
    account_id: h.tenant.accountId,
    added_at_ms: now,
    last_seen_at_ms: now,
  }).execute();
}

export async function connectFailure(
  operation: () => Promise<unknown>,
): Promise<{ code: Code; message: string }> {
  try {
    await operation();
  } catch (error) {
    expect(error).toBeInstanceOf(ConnectError);
    const connectError = error as ConnectError;
    return { code: connectError.code, message: connectError.rawMessage };
  }
  throw new Error("expected ConnectError");
}
