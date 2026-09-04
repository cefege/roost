// This fixture owns isolated coordinator databases and handler dependencies for recovery tests.
// The discovered account-recovery suites create one scope and close it after each test.
// It depends on real migrations, account handlers, encryption, and revocation callbacks.
// Per-file scopes prevent concurrently discovered suites from sharing teardown state.
import { expect } from "bun:test";
import { Code, ConnectError, createContextValues, type HandlerContext } from "@connectrpc/connect";
import { createHash } from "node:crypto";
import type { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  createEmailOutboxPayloadCipher,
  type EmailOutboxPayloadCipher,
} from "@roost/shared/email-payload";
import {
  makeAccountHandlers,
  type AccountHandlers,
} from "../src/connect/handlers-account.ts";
import { callerKey } from "../src/connect/auth-interceptor.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { newJwtCache } from "../src/jwt.ts";

interface RecordingPasswordGate {
  calls: string[];
  hash(password: string): Promise<string>;
}

export interface AccountRecoveryHarness {
  db: KyselyDB;
  sqlite: Database;
  handlers: AccountHandlers;
  cipher: EmailOutboxPayloadCipher;
  passwordGate: RecordingPasswordGate;
  revoked: string[];
  dashboardRevocations: Array<{ dashboardId: string; fingerprint?: string }>;
  callbacksSawCommittedState: boolean[];
  close(): Promise<void>;
}

export interface AccountRecoveryHarnessScope {
  open(options?: { managedContainer?: boolean; instanceId?: string }): Promise<AccountRecoveryHarness>;
  closeAll(): Promise<void>;
}

export const INSTANCE_ID = "11111111-1111-4111-8111-111111111111";
export const ACCOUNT_ID = "22222222-2222-4222-8222-222222222222";
export const TENANT_ROUTE_KEY = "c".repeat(64);
export const MANAGED_WEB_PUBLIC_ORIGIN = "https://dashboard.roosttt.com";
export const OWNER_EMAIL = "owner@example.test";

export function createAccountRecoveryHarnessScope(): AccountRecoveryHarnessScope {
  const cleanups: Array<() => Promise<void>> = [];

  return {
    async open(options = {}): Promise<AccountRecoveryHarness> {
      const directory = mkdtempSync(join(tmpdir(), "roost-account-recovery-"));
      const opened = openDb(join(directory, "test.db"));
      const { db, sqlite } = opened;
      await runMigrations(sqlite);
      const cipher = createEmailOutboxPayloadCipher(OUTBOX_KEY);
      const passwordGate = recordingPasswordGate();
      const revoked: string[] = [];
      const dashboardRevocations: Array<{ dashboardId: string; fingerprint?: string }> = [];
      const callbacksSawCommittedState: boolean[] = [];
      const deps = {
        db,
        sqlite,
        cfg: {
          webPublicUrl: MANAGED_WEB_PUBLIC_ORIGIN,
          managedContainer: options.managedContainer ?? true,
          instanceId: options.instanceId ?? INSTANCE_ID,
          tenantRouteKey: (options.managedContainer ?? true) ? TENANT_ROUTE_KEY : undefined,
        },
        jwtCache: newJwtCache(),
        passwordWorkGate: passwordGate,
        email: { encryptPayload: cipher.encrypt },
        onKeyRevoked: (fingerprint: string): void => {
          revoked.push(fingerprint);
          callbacksSawCommittedState.push(sqlite.query(
            "SELECT fingerprint FROM authorized_keys WHERE fingerprint = ?",
          ).get(fingerprint) === null);
        },
        onDashboardRevoked: (dashboardId: string, fingerprint?: string): void => {
          dashboardRevocations.push({ dashboardId, fingerprint });
          callbacksSawCommittedState.push(sqlite.query(
            "SELECT fingerprint FROM account_devices WHERE fingerprint = ?",
          ).get(fingerprint ?? "") === null);
        },
      } as unknown as ConnectDeps;

      let closed = false;
      const close = async () => {
        if (closed) return;
        closed = true;
        try {
          await opened.close();
        } finally {
          rmSync(directory, { recursive: true, force: true });
        }
      };
      cleanups.push(close);
      return {
        db,
        sqlite,
        handlers: makeAccountHandlers(deps),
        cipher,
        passwordGate,
        revoked,
        dashboardRevocations,
        callbacksSawCommittedState,
        close,
      };
    },

    async closeAll(): Promise<void> {
      for (const close of cleanups.splice(0)) await close();
    },
  };
}

export function tokenHash(token: string): string {
  return createHash("sha256").update(token, "utf8").digest("hex");
}

export function anonymousContext(): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, null);
  return { values } as unknown as HandlerContext;
}

export function authenticatedContext(): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: "authenticated-fingerprint",
    label: "authenticated",
    accountId: "authenticated-account",
  });
  return { values } as unknown as HandlerContext;
}

export async function expectGenericDenial(
  run: () => unknown | Promise<unknown>,
): Promise<void> {
  try {
    await run();
    throw new Error("expected generic denial");
  } catch (error) {
    expect(error).toBeInstanceOf(ConnectError);
    expect((error as ConnectError).code).toBe(Code.PermissionDenied);
    expect((error as ConnectError).message.endsWith("unable to complete request")).toBe(true);
  }
}

export async function expectInvalidPassword(
  run: () => unknown | Promise<unknown>,
): Promise<void> {
  try {
    await run();
    throw new Error("expected invalid password");
  } catch (error) {
    expect(error).toBeInstanceOf(ConnectError);
    expect((error as ConnectError).code).toBe(Code.InvalidArgument);
    expect((error as ConnectError).message.endsWith("invalid password")).toBe(true);
  }
}

export async function captureConsole<T>(
  run: () => T | Promise<T>,
): Promise<{ result: Awaited<T>; output: string }> {
  const output: string[] = [];
  const originalLog = console.log;
  const originalError = console.error;
  console.log = (...args: unknown[]) => output.push(args.map(String).join(" "));
  console.error = (...args: unknown[]) => output.push(args.map(String).join(" "));
  try {
    return { result: await run(), output: output.join("\n") };
  } finally {
    console.log = originalLog;
    console.error = originalError;
  }
}

const OUTBOX_KEY = Buffer.alloc(32, 19).toString("base64");

function recordingPasswordGate(): RecordingPasswordGate {
  const calls: string[] = [];
  return {
    calls,
    async hash(password: string): Promise<string> {
      calls.push(password);
      return `test-hash:${tokenHash(password)}`;
    },
  };
}
