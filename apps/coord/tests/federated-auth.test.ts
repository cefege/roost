import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, createContextValues, type HandlerContext } from "@connectrpc/connect";
import { generateKeyPairSync, randomUUID, sign, type KeyObject } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fingerprintOf } from "@roost/shared/fingerprint";
import {
  AuthFederatedContinueRequestSchema,
  AuthFederatedLinkRequestSchema,
  AuthPasswordAddRequestSchema,
  AuthPasswordLoginRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import { callerKey } from "../src/connect/auth-interceptor.ts";
import {
  FEDERATED_ASSERTION_AUDIENCE,
  FEDERATED_ASSERTION_ISSUER,
  GOOGLE_IDENTITY_ISSUER,
  type FederatedAssertionClaims,
} from "../src/connect/federated-assertion.ts";
import { makeFederatedAuthHandlers } from "../src/connect/handlers-federated-auth.ts";
import { makeNativeAuthHandlers } from "../src/connect/handlers-native-auth.ts";
import { PasswordWorkGate } from "../src/connect/password-work-gate.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { newJwtCache } from "../src/jwt.ts";

const ACCOUNT_ID = "11111111-1111-4111-8111-111111111111";
const COORDINATOR_ID = "22222222-2222-4222-8222-222222222222";
const ROUTE_KEY = "a".repeat(64);
const EMAIL = "owner@example.test";
const GOOGLE_SUBJECT = "google-subject-1";

interface Harness {
  db: KyselyDB;
  deps: ConnectDeps;
  handlers: ReturnType<typeof makeFederatedAuthHandlers>;
  privateKey: KeyObject;
}

const cleanups: Array<() => Promise<void>> = [];
afterEach(async () => {
  for (const cleanup of cleanups.splice(0)) await cleanup();
});

function encodedKey(fill: number): { bytes: Uint8Array; b64: string } {
  const bytes = new Uint8Array(32).fill(fill);
  return { bytes, b64: Buffer.from(bytes).toString("base64") };
}

function openSshPublicKey(raw: Uint8Array): string {
  const type = Buffer.from("ssh-ed25519", "utf8");
  const wire = Buffer.alloc(4 + type.length + 4 + raw.length);
  wire.writeUInt32BE(type.length, 0);
  type.copy(wire, 4);
  wire.writeUInt32BE(raw.length, 4 + type.length);
  Buffer.from(raw).copy(wire, 8 + type.length);
  return `ssh-ed25519 ${wire.toString("base64")} roost-saas-auth\n`;
}

async function harness(): Promise<Harness> {
  const directory = mkdtempSync(join(tmpdir(), "roost-federated-auth-"));
  const opened = openDb(join(directory, "test.db"));
  await runMigrations(opened.sqlite);
  const keyPair = generateKeyPairSync("ed25519");
  const publicDer = keyPair.publicKey.export({ format: "der", type: "spki" }) as Buffer;
  const verifyKeyPath = join(directory, "saas-auth-verify-key");
  writeFileSync(verifyKeyPath, openSshPublicKey(publicDer.subarray(publicDer.length - 32)));
  const deps = {
    db: opened.db,
    sqlite: opened.sqlite,
    cfg: {
      saasMode: true,
      managedContainer: true,
      instanceId: COORDINATOR_ID,
      tenantRouteKey: ROUTE_KEY,
      saasAuthVerifyKeyPath: verifyKeyPath,
    },
    jwtCache: newJwtCache(),
    passwordWorkGate: new PasswordWorkGate(),
    coordKey: {
      sign: async () => "link-ticket",
    },
  } as unknown as ConnectDeps;
  const now = Date.now();
  await opened.db.insertInto("accounts").values({
    id: ACCOUNT_ID, email_normalized: EMAIL, password_hash: null, status: "active",
    created_at_ms: now, password_changed_at_ms: null,
  }).execute();
  await opened.db.insertInto("account_identities").values({
    account_id: ACCOUNT_ID, issuer: GOOGLE_IDENTITY_ISSUER, subject: GOOGLE_SUBJECT,
    email_normalized: EMAIL, linked_at_ms: now, last_authenticated_at_ms: null, revoked_at_ms: null,
  }).execute();
  await opened.db.insertInto("organizations").values({
    id: ACCOUNT_ID, slug: "personal", name: EMAIL, status: "active", created_at_ms: now,
  }).execute();
  await opened.db.insertInto("organization_memberships").values({
    organization_id: ACCOUNT_ID, account_id: ACCOUNT_ID, role: "owner", created_at_ms: now,
  }).execute();
  await opened.db.insertInto("dashboards").values({
    id: COORDINATOR_ID, organization_id: ACCOUNT_ID, slug: "default", name: "Personal",
    status: "active", created_at_ms: now,
  }).execute();
  await opened.db.insertInto("dashboard_memberships").values({
    dashboard_id: COORDINATOR_ID, account_id: ACCOUNT_ID, role: "admin", created_at_ms: now,
  }).execute();
  cleanups.push(async () => {
    try {
      await opened.close();
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });
  return { db: opened.db, deps, handlers: makeFederatedAuthHandlers(deps), privateKey: keyPair.privateKey };
}

function signAssertion(
  privateKey: KeyObject,
  deviceFingerprint: string,
  overrides: Partial<FederatedAssertionClaims> = {},
): string {
  const now = Math.floor(Date.now() / 1_000);
  const payload: FederatedAssertionClaims = {
    iss: FEDERATED_ASSERTION_ISSUER,
    aud: FEDERATED_ASSERTION_AUDIENCE,
    purpose: "continue",
    account_id: ACCOUNT_ID,
    coordinator_id: COORDINATOR_ID,
    route_key: ROUTE_KEY,
    identity_issuer: GOOGLE_IDENTITY_ISSUER,
    identity_subject: GOOGLE_SUBJECT,
    email_normalized: EMAIL,
    device_fp: deviceFingerprint,
    jti: randomUUID(),
    iat: now,
    exp: now + 300,
    ...overrides,
  };
  const header = Buffer.from(JSON.stringify({ alg: "EdDSA", typ: "JWT" })).toString("base64url");
  const body = Buffer.from(JSON.stringify(payload)).toString("base64url");
  const signature = sign(null, Buffer.from(`${header}.${body}`), privateKey).toString("base64url");
  return `${header}.${body}.${signature}`;
}

function anonymousContext(): HandlerContext {
  return { values: createContextValues() } as unknown as HandlerContext;
}

function deviceContext(fingerprint: string): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device", accountId: ACCOUNT_ID, fingerprint, label: "Browser",
  });
  return { values } as unknown as HandlerContext;
}

async function expectUnauthenticated(run: () => Promise<unknown> | unknown): Promise<void> {
  try {
    await run();
    throw new Error("expected unauthenticated result");
  } catch (error) {
    expect(error).toBeInstanceOf(ConnectError);
    expect((error as ConnectError).code).toBe(Code.Unauthenticated);
  }
}

async function continueRequest(h: Harness, fill: number, overrides: Partial<FederatedAssertionClaims> = {}) {
  const key = encodedKey(fill);
  const fingerprint = await fingerprintOf(key.bytes);
  const assertion = signAssertion(h.privateKey, fingerprint, overrides);
  return {
    key,
    fingerprint,
    assertion,
    request: create(AuthFederatedContinueRequestSchema, {
      assertion, sshPubkeyB64: key.b64, label: "Browser",
    }),
  };
}

describe("managed federated credentials", () => {
  test("continues only a preseeded identity and replays the same assertion and key idempotently", async () => {
    const h = await harness();
    const input = await continueRequest(h, 7, { email_normalized: "new-owner@example.test" });
    const first = await h.handlers.authFederatedContinue(input.request, anonymousContext());
    const second = await h.handlers.authFederatedContinue(input.request, anonymousContext());
    expect(first.dashboardId).toBe(COORDINATOR_ID);
    expect(second.dashboardId).toBe(COORDINATOR_ID);
    expect(await h.db.selectFrom("account_devices").selectAll().execute()).toHaveLength(1);
    expect(await h.db.selectFrom("federated_assertion_redemptions").selectAll().execute()).toHaveLength(1);
    expect(await h.db.selectFrom("accounts").select("email_normalized").executeTakeFirstOrThrow())
      .toEqual({ email_normalized: EMAIL });
    expect(await h.db.selectFrom("account_identities").select("email_normalized")
      .where("issuer", "=", GOOGLE_IDENTITY_ISSUER).executeTakeFirstOrThrow())
      .toEqual({ email_normalized: "new-owner@example.test" });
  });

  test("rejects signature, tenant, device, purpose, JTI, expiry, and identity failures uniformly", async () => {
    const cases: Array<Partial<FederatedAssertionClaims>> = [
      { iss: "https://wrong.example/auth" as typeof FEDERATED_ASSERTION_ISSUER },
      { aud: "wrong-audience" as typeof FEDERATED_ASSERTION_AUDIENCE },
      { purpose: "link" },
      { account_id: randomUUID() },
      { coordinator_id: randomUUID() },
      { route_key: "b".repeat(64) },
      { device_fp: "c".repeat(64) },
      { jti: "not-a-uuid" },
      { iat: 1, exp: 2 },
      { identity_subject: "unknown-google-subject" },
    ];
    for (const [index, overrides] of cases.entries()) {
      const h = await harness();
      const input = await continueRequest(h, 20 + index, overrides);
      await expectUnauthenticated(() => h.handlers.authFederatedContinue(input.request, anonymousContext()));
      expect(await h.db.selectFrom("account_devices").selectAll().execute()).toEqual([]);
    }
    const wrongSignature = await harness();
    const input = await continueRequest(wrongSignature, 40);
    const [header, payload, signature] = input.assertion.split(".");
    if (!header || !payload || !signature) throw new Error("fixture assertion is malformed");
    const changedSignature = `${signature[0] === "A" ? "B" : "A"}${signature.slice(1)}`;
    const tampered = create(AuthFederatedContinueRequestSchema, {
      assertion: `${header}.${payload}.${changedSignature}`,
      sshPubkeyB64: input.key.b64,
      label: "Browser",
    });
    await expectUnauthenticated(() => wrongSignature.handlers.authFederatedContinue(tampered, anonymousContext()));

    const revoked = await harness();
    await revoked.db.updateTable("account_identities").set({ revoked_at_ms: Date.now() })
      .where("issuer", "=", GOOGLE_IDENTITY_ISSUER).execute();
    const revokedInput = await continueRequest(revoked, 41);
    await expectUnauthenticated(() => revoked.handlers.authFederatedContinue(revokedInput.request, anonymousContext()));
  });

  test("adds a native password and binds link assertions to the authenticated device", async () => {
    const h = await harness();
    const input = await continueRequest(h, 50);
    await h.handlers.authFederatedContinue(input.request, anonymousContext());
    const context = deviceContext(input.fingerprint);
    await h.handlers.authPasswordAdd(create(AuthPasswordAddRequestSchema, {
      newPassword: "a secure added password",
    }), context);
    expect(await h.db.selectFrom("account_identities").selectAll()
      .where("issuer", "=", "native").executeTakeFirstOrThrow()).toMatchObject({
      account_id: ACCOUNT_ID, subject: ACCOUNT_ID, email_normalized: EMAIL, revoked_at_ms: null,
    });

    const loginKey = encodedKey(51);
    const login = await makeNativeAuthHandlers(h.deps).authPasswordLogin(
      create(AuthPasswordLoginRequestSchema, {
        email: EMAIL, password: "a secure added password", sshPubkeyB64: loginKey.b64, label: "Second browser",
      }),
      anonymousContext(),
    );
    expect(login.dashboardId).toBe(COORDINATOR_ID);

    const linkAssertion = signAssertion(h.privateKey, input.fingerprint, {
      purpose: "link", jti: randomUUID(),
    });
    const linkRequest = create(AuthFederatedLinkRequestSchema, { assertion: linkAssertion });
    await expect(h.handlers.authFederatedLink(linkRequest, context)).resolves.toMatchObject({ ok: true });
    await expect(h.handlers.authFederatedLink(linkRequest, context)).resolves.toMatchObject({ ok: true });
    const wrongDevice = signAssertion(h.privateKey, "d".repeat(64), { purpose: "link" });
    await expectUnauthenticated(() => h.handlers.authFederatedLink(
      create(AuthFederatedLinkRequestSchema, { assertion: wrongDevice }), context,
    ));
  });
});
