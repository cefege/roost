// Owns the isolated database harness shared by native password login suites.
// The sibling credential and device-key tests create one fixture instance per file.
// Keeping cleanup state per instance prevents either suite from closing the other's database.
// It depends on coord migrations, native auth handlers, and Bun password hashing.
import { expect } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, createContextValues, type HandlerContext } from "@connectrpc/connect";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { NATIVE_PASSWORD_ARGON2ID } from "@roost/shared/native-credentials";
import { AuthPasswordLoginRequestSchema } from "@roost/shared/proto/coordinator_pb";
import {
  makeNativeAuthHandlers,
  type NativeAuthHandlerOptions,
  type NativeAuthHandlers,
} from "../src/connect/handlers-native-auth.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { PasswordWorkGate } from "../src/connect/password-work-gate.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { newJwtCache } from "../src/jwt.ts";

export const ACCOUNT_ID = "account-owner";
export const OTHER_ACCOUNT_ID = "account-other";
export const ORGANIZATION_ID = "organization-managed";
export const DASHBOARD_ID = "dashboard-managed";
export const EMAIL = "owner@example.test";
export const PASSWORD = "correct horse battery staple";
export const NOW = 1_900_000_000_000;

export interface Harness {
  db: KyselyDB;
  deps: ConnectDeps;
  handlers: NativeAuthHandlers;
}

export function handlerContext(): HandlerContext {
  return { values: createContextValues() } as unknown as HandlerContext;
}

export function encodedKey(fill: number): { bytes: Uint8Array; b64: string } {
  const bytes = new Uint8Array(32).fill(fill);
  return { bytes, b64: Buffer.from(bytes).toString("base64") };
}

export function loginRequest(keyB64: string, overrides: Partial<{
  email: string;
  password: string;
  sshPubkeyB64: string;
  label: string;
}> = {}) {
  return create(AuthPasswordLoginRequestSchema, {
    email: overrides.email ?? EMAIL,
    password: overrides.password ?? PASSWORD,
    sshPubkeyB64: overrides.sshPubkeyB64 ?? keyB64,
    label: overrides.label ?? "Owner browser",
  });
}

export async function expectInvalidCredentials(
  run: () => unknown | Promise<unknown>,
): Promise<void> {
  try {
    await run();
    throw new Error("expected invalid credentials");
  } catch (error) {
    expect(error).toBeInstanceOf(ConnectError);
    expect((error as ConnectError).code).toBe(Code.Unauthenticated);
    expect((error as ConnectError).message.endsWith("invalid credentials")).toBe(true);
  }
}

export async function expectNoDeviceWrites(db: KyselyDB): Promise<void> {
  expect(await db.selectFrom("account_devices").select("fingerprint").execute()).toEqual([]);
  expect(await db.selectFrom("authorized_keys").select("fingerprint").execute()).toEqual([]);
}

export function createNativePasswordLoginFixtures() {
  const cleanups: Array<() => Promise<void>> = [];
  let passwordHash = "";

  async function initializePasswordHash(): Promise<void> {
    passwordHash = await Bun.password.hash(PASSWORD, NATIVE_PASSWORD_ARGON2ID);
  }

  async function cleanupHarnesses(): Promise<void> {
    for (const close of cleanups.splice(0)) await close();
  }

  async function createHarness(options: NativeAuthHandlerOptions = {}): Promise<Harness> {
    const directory = mkdtempSync(join(tmpdir(), "roost-native-login-"));
    const opened = openDb(join(directory, "test.db"));
    await runMigrations(opened.sqlite);
    const deps = {
      db: opened.db,
      sqlite: opened.sqlite,
      cfg: { saasMode: true },
      jwtCache: newJwtCache(),
      passwordWorkGate: new PasswordWorkGate(),
    } as unknown as ConnectDeps;
    cleanups.push(async () => {
      try {
        await opened.close();
      } finally {
        rmSync(directory, { recursive: true, force: true });
      }
    });
    return {
      db: opened.db,
      deps,
      handlers: makeNativeAuthHandlers(deps, options),
    };
  }

  async function provisionOwner(
    db: KyselyDB,
    options: { status?: "active" | "disabled"; storedHash?: string | null } = {},
  ): Promise<void> {
    await db.insertInto("accounts").values({
      id: ACCOUNT_ID,
      email_normalized: EMAIL,
      password_hash: options.storedHash === undefined ? passwordHash : options.storedHash,
      status: options.status ?? "active",
      created_at_ms: NOW,
      password_changed_at_ms: NOW,
    }).execute();
    await db.insertInto("account_identities").values({
      account_id: ACCOUNT_ID,
      issuer: "native",
      subject: ACCOUNT_ID,
      email_normalized: EMAIL,
      linked_at_ms: NOW,
      last_authenticated_at_ms: null,
      revoked_at_ms: null,
    }).execute();
    await db.insertInto("organizations").values({
      id: ORGANIZATION_ID,
      slug: "managed",
      name: "Managed",
      status: "active",
      created_at_ms: NOW,
    }).execute();
    await db.insertInto("organization_memberships").values({
      organization_id: ORGANIZATION_ID,
      account_id: ACCOUNT_ID,
      role: "owner",
      created_at_ms: NOW,
    }).execute();
    await db.insertInto("dashboards").values({
      id: DASHBOARD_ID,
      organization_id: ORGANIZATION_ID,
      slug: "default",
      name: "Default",
      status: "active",
      created_at_ms: NOW,
    }).execute();
    await db.insertInto("dashboard_memberships").values({
      dashboard_id: DASHBOARD_ID,
      account_id: ACCOUNT_ID,
      role: "admin",
      created_at_ms: NOW,
    }).execute();
  }

  function getPasswordHash(): string {
    return passwordHash;
  }

  return {
    cleanupHarnesses,
    createHarness,
    getPasswordHash,
    initializePasswordHash,
    provisionOwner,
  };
}
