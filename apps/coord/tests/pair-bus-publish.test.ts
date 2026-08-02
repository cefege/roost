// Regression guard for the pair-request firehose delta (perf sweep C2.4 —
// replaced the SPA's 5 s pairList poller): every pair_requests mutation MUST
// publish a pairBus delta, or already-trusted browsers go stale until the
// next Sync reconnect (the snapshot seed). Contract: pairCreate → `pending`,
// pairApprove/pairDeny → `removed`. Drives the REAL handlers + REAL pairBus
// over an in-memory DB (pattern: task-bus-publish.test.ts).

import { describe, test, expect, beforeAll, afterAll } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { create } from "@bufbuild/protobuf";
import { createContextValues, Code, ConnectError } from "@connectrpc/connect";
import type { HandlerContext, ServiceImpl } from "@connectrpc/connect";
import {
  CoordinatorService,
  PairCreateRequestSchema, PairApproveRequestSchema, PairDenyRequestSchema,
  PairListRequestSchema,
  type PairCreateResponse, type PairApproveResponse,
  type PairDenyResponse, type PairListResponse,
} from "@roost/shared/proto/coordinator_pb";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { makeAuthHandlers } from "../src/connect/handlers-auth.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { pairBus, type PairRequestDelta } from "../src/buses.ts";
import { onHostKey, remoteAddressKey } from "../src/connect/auth-interceptor.ts";
import { fingerprintOf } from "../src/jwt.ts";

// The generated ServiceImpl types handler returns as MessageInitShape | Promise<…>
// (sync-or-async, partial-field). These handlers are all `async` and return
// concrete `create(Schema, …)` messages — model that: reuse the generated
// parameter types, but pin returns to Promise<ConcreteResponse>. Otherwise an
// awaited response has optional fields (ephemeralId/requests) and the call
// isn't a Promise to .catch().
type PairMethod = "pairCreate" | "pairApprove" | "pairDeny" | "pairList";
type PairResponse = {
  pairCreate: PairCreateResponse;
  pairApprove: PairApproveResponse;
  pairDeny: PairDenyResponse;
  pairList: PairListResponse;
};
type PairHandlers = {
  [K in PairMethod]: (
    ...args: Parameters<ServiceImpl<typeof CoordinatorService>[K]>
  ) => Promise<PairResponse[K]>;
};

let workdir: string;
let closeDb: () => void;
let handlers: PairHandlers;
let pubkeyB64: string;
let testDb: KyselyDB;

// requireAuth only reads ctx.values.get(callerKey); a fake caller suffices —
// building a real HandlerContext would drag in the whole transport.
const authCtx = {
  values: { get: () => ({ fingerprint: "fp-test" }) },
} as unknown as HandlerContext;

// Unauthenticated ctx with a real ContextValues bag: callerKey stays at its
// default (null) so optionalAuth() returns null and the handler falls back to
// the loopback check on remoteAddressKey.
function remoteCtx(addr: string | undefined, onHost = false): HandlerContext {
  const values = createContextValues();
  if (addr !== undefined) values.set(remoteAddressKey, addr);
  values.set(onHostKey, onHost);
  return { values } as unknown as HandlerContext;
}

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-pairbus-"));
  const { db, sqlite } = openDb(join(workdir, "test.db"));
  testDb = db;
  await runMigrations(sqlite);
  await db.insertInto("authorized_keys").values({
    fingerprint: "fp-test",
    public_key: new Uint8Array(32),
    label: "approver",
    added_at: Date.now(),
  }).execute();
  closeDb = () => { try { sqlite.close(); } catch { /* ignore */ } };
  // The pair handlers touch only deps.db; the remaining ConnectDeps surface
  // (coordKey etc.) is irrelevant here.
  handlers = makeAuthHandlers({ db } as unknown as ConnectDeps) as unknown as PairHandlers;

  const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const rawPub = new Uint8Array(await crypto.subtle.exportKey("raw", keys.publicKey));
  pubkeyB64 = Buffer.from(rawPub).toString("base64");
});

afterAll(() => { closeDb?.(); rmSync(workdir, { recursive: true, force: true }); });

function capture(): { msgs: PairRequestDelta[]; stop: () => void } {
  const msgs: PairRequestDelta[] = [];
  const stop = pairBus.subscribe((m) => msgs.push(m));
  return { msgs, stop };
}

async function createRequest(label: string): Promise<string> {
  const resp = await handlers.pairCreate(
    create(PairCreateRequestSchema, { sshPubkeyB64: pubkeyB64, label }), authCtx);
  return resp.ephemeralId;
}

describe("pair mutations publish pairBus deltas", () => {
  test("pairCreate publishes a pending delta", async () => {
    const cap = capture();
    const eid = await createRequest("test-browser");
    cap.stop();
    const pending = cap.msgs.find((m) => m.kind === "pending" && m.ephemeral_id === eid);
    expect(pending).toBeDefined();
    if (pending?.kind === "pending") {
      expect(pending.label).toBe("test-browser");
      expect(pending.created_at_ms).toBeGreaterThan(0);
    }
  });

  test("pairApprove publishes a removed delta", async () => {
    const eid = await createRequest("approve-me");
    const cap = capture();
    await handlers.pairApprove(create(PairApproveRequestSchema, { ephemeralId: eid }), authCtx);
    cap.stop();
    expect(cap.msgs.some((m) => m.kind === "removed" && m.ephemeral_id === eid)).toBe(true);
  });

  test("pairDeny publishes a removed delta", async () => {
    const eid = await createRequest("deny-me");
    const cap = capture();
    await handlers.pairDeny(create(PairDenyRequestSchema, { ephemeralId: eid }), authCtx);
    cap.stop();
    expect(cap.msgs.some((m) => m.kind === "removed" && m.ephemeral_id === eid)).toBe(true);
  });
});

// Loopback API trust path (Author 2026-07-11 "approve new devices via API"):
// pairList/pairApprove/pairDeny accept an UNAUTHENTICATED caller from
// loopback so the on-host agent/CLI can drive device approval. NOT
// tailnet-wide — a tailnet device could otherwise pairCreate + self-approve.
describe("pair loopback API trust path", () => {
  test("pairApprove from loopback without JWT approves + authorizes the key", async () => {
    const eid = await createRequest("api-approve-me");
    const resp = await handlers.pairApprove(
      create(PairApproveRequestSchema, { ephemeralId: eid }), remoteCtx("127.0.0.1", true));
    expect(resp.ok).toBe(true);
    const row = await testDb.selectFrom("pair_requests").select(["status"])
      .where("ephemeral_id", "=", eid).executeTakeFirst();
    expect(row?.status).toBe("approved");
    const authorized = await testDb.selectFrom("authorized_keys").select(["label"])
      .where("label", "=", "api-approve-me").executeTakeFirst();
    expect(authorized).toBeDefined();
  });

  test("pairDeny from loopback without JWT denies", async () => {
    const eid = await createRequest("api-deny-me");
    const resp = await handlers.pairDeny(
      create(PairDenyRequestSchema, { ephemeralId: eid }), remoteCtx("127.0.0.1", true));
    expect(resp.ok).toBe(true);
  });

  test("pairList from loopback without JWT lists pending", async () => {
    const eid = await createRequest("api-list-me");
    const resp = await handlers.pairList(
      create(PairListRequestSchema, {}), remoteCtx("::1", true));
    expect(resp.requests.some((r) => r.ephemeralId === eid)).toBe(true);
    // cleanup so later runs of this file start pending-free
    await handlers.pairDeny(create(PairDenyRequestSchema, { ephemeralId: eid }), remoteCtx("127.0.0.1", true));
  });

  test("unauthenticated tailnet addr is rejected (no self-approval)", async () => {
    const eid = await createRequest("tailnet-reject-me");
    await expect(handlers.pairApprove(
      create(PairApproveRequestSchema, { ephemeralId: eid }), remoteCtx("100.101.102.103", false),
    ).catch((e: ConnectError) => e.code)).resolves.toBe(Code.PermissionDenied);
    await handlers.pairDeny(create(PairDenyRequestSchema, { ephemeralId: eid }), remoteCtx("127.0.0.1", true));
  });

  test("unauthenticated with no remote address is rejected", async () => {
    const eid = await createRequest("noaddr-reject-me");
    await expect(handlers.pairDeny(
      create(PairDenyRequestSchema, { ephemeralId: eid }), remoteCtx(undefined, false),
    ).catch((e: ConnectError) => e.code)).resolves.toBe(Code.PermissionDenied);
    await handlers.pairDeny(create(PairDenyRequestSchema, { ephemeralId: eid }), remoteCtx("127.0.0.1", true));
  });

  test("authed caller still works regardless of remote address (notifier path)", async () => {
    const eid = await createRequest("authed-still-works");
    const resp = await handlers.pairDeny(
      create(PairDenyRequestSchema, { ephemeralId: eid }), authCtx);
    expect(resp.ok).toBe(true);
  });

  test("an authenticated approval cannot resume after its approver is revoked", async () => {
    const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
    const raw = new Uint8Array(await crypto.subtle.exportKey("raw", keys.publicKey));
    const proposedFp = await fingerprintOf(raw);
    const created = await handlers.pairCreate(create(PairCreateRequestSchema, {
      sshPubkeyB64: Buffer.from(raw).toString("base64"),
      label: "paused-approval",
    }), authCtx);
    await testDb.transaction().execute(async (trx) => {
      await trx.insertInto("authorized_key_revocations").values({
        fingerprint: "fp-test",
        revoked_at_ms: Date.now(),
        revoked_by_fp: "test",
        reason: "test",
      }).execute();
      await trx.deleteFrom("authorized_keys").where("fingerprint", "=", "fp-test").execute();
    });
    await expect(handlers.pairApprove(
      create(PairApproveRequestSchema, { ephemeralId: created.ephemeralId }),
      authCtx,
    )).rejects.toMatchObject({ code: Code.NotFound });
    expect(await testDb.selectFrom("pair_requests").select("status")
      .where("ephemeral_id", "=", created.ephemeralId).executeTakeFirstOrThrow())
      .toEqual({ status: "pending" });
    expect(await testDb.selectFrom("authorized_keys").select("fingerprint")
      .where("fingerprint", "=", proposedFp).executeTakeFirst()).toBeUndefined();
  });
});
