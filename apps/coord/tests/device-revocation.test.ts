import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, createContextValues, type HandlerContext } from "@connectrpc/connect";
import {
  AuthRedeemBrowserRequestSchema,
  AuthRedeemWorkerRequestSchema,
  DevicesListRequestSchema,
  DevicesRevokeRequestSchema,
  DevicesRotateCurrentRequestSchema,
  WorkersDeleteRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { makeAuthHandlers } from "../src/connect/handlers-auth.ts";
import { onHostKey, remoteAddressKey } from "../src/connect/auth-interceptor.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { makeWorkerHandlers } from "../src/connect/handlers-workers.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { newJwtCache } from "../src/jwt.ts";

interface Harness {
  db: KyselyDB;
  handlers: ReturnType<typeof makeAuthHandlers>;
  workerHandlers: ReturnType<typeof makeWorkerHandlers>;
  revoked: string[];
  close(): Promise<void>;
}

const cleanups: Array<() => Promise<void>> = [];
afterEach(async () => {
  for (const cleanup of cleanups.splice(0)) await cleanup();
});

async function harness(): Promise<Harness> {
  const dir = mkdtempSync(join(tmpdir(), "roost-device-revoke-"));
  const opened = openDb(join(dir, "test.db"));
  const { db, sqlite } = opened;
  await runMigrations(sqlite);
  const revoked: string[] = [];
  const deps = {
    db,
    sqlite,
    coordKey: { verifyingKeyB64: () => "" },
    jwtCache: newJwtCache(),
    onKeyRevoked: (fingerprint: string) => revoked.push(fingerprint),
  } as unknown as ConnectDeps;
  const close = async () => {
    try { await opened.close(); } finally { rmSync(dir, { recursive: true, force: true }); }
  };
  cleanups.push(close);
  return {
    db,
    handlers: makeAuthHandlers(deps),
    workerHandlers: makeWorkerHandlers(deps),
    revoked,
    close,
  };
}

function authCtx(fingerprint: string): HandlerContext {
  return {
    values: { get: () => ({ fingerprint, label: "test" }) },
  } as unknown as HandlerContext;
}

function unauthCtx(address: string, onHost: boolean): HandlerContext {
  const values = createContextValues();
  values.set(remoteAddressKey, address);
  values.set(onHostKey, onHost);
  return { values } as unknown as HandlerContext;
}

async function key(): Promise<{ raw: Uint8Array; b64: string; fingerprint: string }> {
  const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const raw = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  return {
    raw,
    b64: Buffer.from(raw).toString("base64"),
    fingerprint: await fingerprintOf(raw),
  };
}

async function authorize(db: KyselyDB, device: Awaited<ReturnType<typeof key>>, label: string): Promise<void> {
  await db.insertInto("authorized_keys").values({
    fingerprint: device.fingerprint,
    public_key: device.raw,
    label,
    added_at: Date.now(),
  }).execute();
}

async function token(
  db: KyselyDB,
  value: string,
  minter: string | null,
  kind = "browser",
): Promise<void> {
  await db.insertInto("bootstrap_tokens").values({
    token: value,
    kind,
    label: "new browser",
    created_at_ms: Date.now(),
    expires_at_ms: Date.now() + 60_000,
    used_at_ms: null,
    used_by_fp: null,
    minted_by_fp: minter,
  }).execute();
}

describe("authorized device lifecycle", () => {
  test("lists non-worker devices newest-first and marks the caller", async () => {
    const h = await harness();
    const self = await key();
    const peer = await key();
    await authorize(h.db, self, "self");
    await authorize(h.db, peer, "peer");
    await h.db.updateTable("authorized_keys").set({ added_at: 2 })
      .where("fingerprint", "=", self.fingerprint).execute();
    await h.db.updateTable("authorized_keys").set({ added_at: 1 })
      .where("fingerprint", "=", peer.fingerprint).execute();
    await h.db.insertInto("workers").values({
      fp: peer.fingerprint,
      label: "worker",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: 1,
      last_seen_ms: 1,
      reachable_addr: null,
      keeper_stale: null,
    }).execute();

    const response = await h.handlers.devicesList(
      create(DevicesListRequestSchema),
      authCtx(self.fingerprint),
    );
    const devices = response.devices ?? [];
    expect(devices.map((device) => device.fingerprint)).toEqual([self.fingerprint]);
    expect(devices[0]?.isSelf).toBe(true);
  });

  test("rejects authenticated self-revoke and unauthenticated proxy callers", async () => {
    const h = await harness();
    const self = await key();
    await authorize(h.db, self, "self");
    const request = create(DevicesRevokeRequestSchema, { fingerprint: self.fingerprint });
    await expect(h.handlers.devicesRevoke(request, authCtx(self.fingerprint)))
      .rejects.toMatchObject({ code: Code.InvalidArgument });
    await expect(h.handlers.devicesRevoke(request, unauthCtx("100.64.0.2", false)))
      .rejects.toMatchObject({ code: Code.PermissionDenied });
  });

  test("on-host recovery permanently revokes a device and all uncertain tokens", async () => {
    const h = await harness();
    const lost = await key();
    await authorize(h.db, lost, "lost");
    await token(h.db, "delegated", lost.fingerprint);
    await token(h.db, "legacy", null);

    const response = await h.handlers.devicesRevoke(
      create(DevicesRevokeRequestSchema, { fingerprint: lost.fingerprint }),
      unauthCtx("127.0.0.1", true),
    );
    expect(response.ok).toBe(true);
    expect(await h.db.selectFrom("authorized_keys").selectAll().execute()).toEqual([]);
    expect(await h.db.selectFrom("bootstrap_tokens").selectAll().execute()).toEqual([]);
    expect(await h.db.selectFrom("authorized_key_revocations").selectAll().executeTakeFirst())
      .toMatchObject({ fingerprint: lost.fingerprint, revoked_by_fp: "on-host-recovery" });
    expect(h.revoked).toEqual([lost.fingerprint]);
  });

  test("rotates atomically and invalidates the old key's delegated tokens", async () => {
    const h = await harness();
    const old = await key();
    const replacement = await key();
    await authorize(h.db, old, "old");
    await token(h.db, "delegated", old.fingerprint);

    const response = await h.handlers.devicesRotateCurrent(
      create(DevicesRotateCurrentRequestSchema, {
        sshPubkeyB64: replacement.b64,
        label: "replacement",
      }),
      authCtx(old.fingerprint),
    );
    expect(response.fingerprint).toBe(replacement.fingerprint);
    expect(await h.db.selectFrom("authorized_keys").select("fingerprint").execute())
      .toEqual([{ fingerprint: replacement.fingerprint }]);
    expect(await h.db.selectFrom("authorized_key_revocations").select("fingerprint").execute())
      .toEqual([{ fingerprint: old.fingerprint }]);
    expect(await h.db.selectFrom("bootstrap_tokens").selectAll().execute()).toEqual([]);
    expect(h.revoked).toEqual([old.fingerprint]);
  });

  test("rotation rejects malformed SSH wire keys instead of slicing arbitrary bytes", async () => {
    const h = await harness();
    const current = await key();
    await authorize(h.db, current, "current");
    const malformedWire = Buffer.from(new Uint8Array(51)).toString("base64");
    await expect(h.handlers.devicesRotateCurrent(
      create(DevicesRotateCurrentRequestSchema, {
        sshPubkeyB64: malformedWire,
        label: "malformed",
      }),
      authCtx(current.fingerprint),
    )).rejects.toMatchObject({ code: Code.InvalidArgument });
    expect(await h.db.selectFrom("authorized_keys").select("fingerprint").execute())
      .toEqual([{ fingerprint: current.fingerprint }]);
  });

  test("two browser redemptions race: exactly one claims and installs a key", async () => {
    const h = await harness();
    const first = await key();
    const second = await key();
    await token(h.db, "one-shot", null);
    const redeem = (device: Awaited<ReturnType<typeof key>>) => h.handlers.authRedeemBrowser(
      create(AuthRedeemBrowserRequestSchema, {
        token: "one-shot",
        sshPubkeyB64: device.b64,
        label: "racer",
      }),
      unauthCtx("203.0.113.1", false),
    );
    const settled = await Promise.allSettled([redeem(first), redeem(second)]);
    expect(settled.filter((result) => result.status === "fulfilled")).toHaveLength(1);
    expect(settled.filter((result) => result.status === "rejected")).toHaveLength(1);
    const authorized = await h.db.selectFrom("authorized_keys").select("fingerprint").execute();
    expect(authorized).toHaveLength(1);
    expect([first.fingerprint, second.fingerprint]).toContain(authorized[0]?.fingerprint);
    const claimed = await h.db.selectFrom("bootstrap_tokens").select("used_by_fp")
      .where("token", "=", "one-shot").executeTakeFirstOrThrow();
    expect(claimed.used_by_fp).toBe(authorized[0]?.fingerprint);
  });

  test("two worker redemptions race: exactly one claims and registers", async () => {
    const h = await harness();
    const first = await key();
    const second = await key();
    await token(h.db, "worker-one-shot", null, "worker");
    const redeem = (device: Awaited<ReturnType<typeof key>>) => h.handlers.authRedeemWorker(
      create(AuthRedeemWorkerRequestSchema, {
        token: "worker-one-shot",
        sshPubkeyB64: device.b64,
        label: "worker-racer",
        os: "linux",
      }),
      unauthCtx("203.0.113.1", false),
    );
    const settled = await Promise.allSettled([redeem(first), redeem(second)]);
    expect(settled.filter((result) => result.status === "fulfilled")).toHaveLength(1);
    expect(settled.filter((result) => result.status === "rejected")).toHaveLength(1);
    const workers = await h.db.selectFrom("workers").select("fp").execute();
    expect(workers).toHaveLength(1);
    expect([first.fingerprint, second.fingerprint]).toContain(workers[0]?.fp);
    const claimed = await h.db.selectFrom("bootstrap_tokens").select("used_by_fp")
      .where("token", "=", "worker-one-shot").executeTakeFirstOrThrow();
    expect(claimed.used_by_fp).toBe(workers[0]?.fp);
  });

  test("worker deletion tombstones the key and invalidates delegated tokens", async () => {
    const h = await harness();
    const worker = await key();
    await authorize(h.db, worker, "worker");
    await h.db.insertInto("workers").values({
      fp: worker.fingerprint,
      label: "worker",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: 1,
      last_seen_ms: 1,
      reachable_addr: null,
      keeper_stale: null,
    }).execute();
    await token(h.db, "worker-delegated", worker.fingerprint);
    await token(h.db, "legacy-uncertain", null);

    const response = await h.workerHandlers.workersDelete(
      create(WorkersDeleteRequestSchema, { fp: worker.fingerprint }),
      authCtx("administrator"),
    );
    expect(response.ok).toBe(true);
    expect(await h.db.selectFrom("workers").selectAll().execute()).toEqual([]);
    expect(await h.db.selectFrom("authorized_keys").selectAll().execute()).toEqual([]);
    expect(await h.db.selectFrom("bootstrap_tokens").selectAll().execute()).toEqual([]);
    expect(await h.db.selectFrom("authorized_key_revocations").select("fingerprint").execute())
      .toEqual([{ fingerprint: worker.fingerprint }]);
    await expect(h.db.insertInto("authorized_keys").values({
      fingerprint: worker.fingerprint,
      public_key: worker.raw,
      label: "reconnect",
      added_at: Date.now(),
    }).execute()).rejects.toThrow("authorized key revoked");
    expect(h.revoked).toEqual([worker.fingerprint]);
  });
});
