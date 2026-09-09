// Proves keeper-update preparation is mutually exclusive with session creation.
// The fixture drives authenticated Connect handlers through createCoord and a
// real write gate while a fake routable worker controls result completion.
// Deferred signals expose dispatch and database-read boundaries without sleeps.

import { afterEach, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import type { CoordConfig } from "@roost/shared/config";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";
import { newJwtCache, signJwt } from "../src/jwt.ts";
import { createCoord } from "../src/coord-factory.ts";
import {
  CoordinatorWriteGate,
  type WriteLease,
} from "../src/coordinator-write-gate.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import {
  rejectPendingRpc,
  rejectPendingRpcsForWorker,
  resolvePendingRpc,
} from "../src/router/pending-rpcs.ts";
import { resetPendingSpawnsForTest } from "../src/connect/pending-spawns.ts";

const WORKER_FP = "ab".repeat(32);
const FIRST_SESSION_ID = "10000000-0000-4000-8000-000000000001";
const BLOCKED_SESSION_ID = "10000000-0000-4000-8000-000000000002";
const AFTER_SUCCESS_SESSION_ID = "10000000-0000-4000-8000-000000000003";
const AFTER_FAILURE_SESSION_ID = "10000000-0000-4000-8000-000000000004";

interface VoidDeferred {
  promise: Promise<void>;
  resolve(): void;
}

interface KeeperFenceHarness {
  order: string[];
  gate: ObservedWriteGate;
  rpc(method: string, body: unknown): Promise<Response>;
  finalEmptyRecheck: Promise<void>;
  keeperDispatched: Promise<void>;
  keeperDispatchCount(): number;
  keeperRequestId(): string | null;
  spawnRequestId(sessionId: string): string | undefined;
  waitForSpawn(sessionId: string): Promise<void>;
  close(): Promise<void>;
}

class ObservedWriteGate extends CoordinatorWriteGate {
  readonly exclusiveRequested = Promise.withResolvers<void>();
  readonly exclusiveAcquired = Promise.withResolvers<void>();
  normalLeaseCount = 0;

  constructor(private readonly order: string[]) { super(); }

  override acquire(): WriteLease {
    let lease: WriteLease;
    try {
      lease = super.acquire();
    } catch (error) {
      this.order.push(`normal-rejected:exclusive=${this.exclusive}`);
      throw error;
    }
    this.normalLeaseCount += 1;
    this.order.push(`normal-acquired:${this.normalLeaseCount}`);
    let released = false;
    return {
      release: () => {
        if (released) return;
        released = true;
        this.normalLeaseCount -= 1;
        this.order.push(`normal-released:${this.normalLeaseCount}`);
        lease.release();
      },
    };
  }

  override async acquireExclusive(owner: string, timeoutMs?: number): Promise<WriteLease> {
    this.order.push(`exclusive-requested:normal=${this.normalLeaseCount}`);
    this.exclusiveRequested.resolve();
    const lease = await super.acquireExclusive(owner, timeoutMs);
    this.order.push(`exclusive-held:normal=${this.normalLeaseCount}`);
    this.exclusiveAcquired.resolve();
    let released = false;
    return {
      release: () => {
        if (released) return;
        released = true;
        this.order.push("exclusive-released");
        lease.release();
      },
    };
  }
}

async function openHarness(): Promise<KeeperFenceHarness> {
  const directory = mkdtempSync(join(tmpdir(), "roost-keeper-fence-"));
  const opened = openDb(join(directory, "coord.db"));
  await runMigrations(opened.sqlite);
  const selfHostedTenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  const browserKeys = await crypto.subtle.generateKey(
    { name: "Ed25519" }, true, ["sign", "verify"],
  );
  const publicKey = new Uint8Array(await crypto.subtle.exportKey("raw", browserKeys.publicKey));
  const browserFp = await fingerprintOf(publicKey);
  const now = Date.now();
  await opened.db.insertInto("authorized_keys").values({
    fingerprint: browserFp, public_key: publicKey, label: "keeper-fence-browser", added_at: now,
  }).execute();
  await opened.db.insertInto("account_devices").values({
    fingerprint: browserFp, account_id: selfHostedTenant.accountId,
    added_at_ms: now, last_seen_at_ms: now,
  }).execute();
  await opened.db.insertInto("workers").values({
    fp: WORKER_FP, dashboard_id: selfHostedTenant.dashboardId, label: "keeper-fence-worker", os: "linux",
    reachable_addr: "127.0.0.1", git_sha: null, host_metrics_json: null,
    registered_at_ms: now, last_seen_ms: now,
  }).execute();

  const order: string[] = [];
  const gate = new ObservedWriteGate(order);
  const finalEmptyRecheck = Promise.withResolvers<void>();
  const cfg: CoordConfig = {
    trustProxy: false, bind: "127.0.0.1:0",
    pushAllowedOrigins: [], dbPath: join(directory, "coord.db"),
    authorizedKeysPath: join(directory, "keys"),
    webDistPath: "", jwtMaxAgeSecs: 300,
    auditRetentionDays: 90, relaxedCsp: false, corsAllowedOrigins: [], logDir: directory,
    publicUrl: undefined,
  };
  const coord = createCoord({
    db: opened.db,
    sqlite: opened.sqlite,
    cfg,
    jwtCache: newJwtCache(),
    writeGate: gate,
    selfHostedTenant,
    _onKeeperUpdateFinalEmptyRecheck: () => {
      order.push(`final-empty-recheck:exclusive=${gate.exclusiveHeld}`);
      finalEmptyRecheck.resolve();
    },
  });
  const issuedAt = Math.floor(now / 1000);
  const jwt = await signJwt(
    { aud: "roost-coordinator", sub: browserFp, iat: issuedAt, exp: issuedAt + 60 },
    browserKeys.privateKey, browserFp,
  );

  const spawnRequests = new Map<string, string>();
  const spawnWaiters = new Map<string, VoidDeferred>();
  const keeperDispatched = Promise.withResolvers<void>();
  let keeperRequestId: string | null = null;
  let keeperDispatchCount = 0;
  __setConnectWorkerForTest(WORKER_FP, {
    workerFp: WORKER_FP,
    send(frame: CoordWorkerDown): number {
      if (frame.frame.case === "browserCommand") {
        const command = JSON.parse(frame.frame.value.frameJson) as {
          kind?: string; session_id?: string;
        };
        if (command.kind === "spawn-shell" && command.session_id) {
          spawnRequests.set(command.session_id, frame.frame.value.requestId);
          order.push(`spawn-dispatched:${command.session_id}:normal=${gate.normalLeaseCount}`);
          spawnWaiters.get(command.session_id)?.resolve();
        }
      } else if (frame.frame.case === "keeperUpdatePrepare") {
        keeperRequestId = frame.frame.value.requestId;
        keeperDispatchCount += 1;
        order.push(`shutdown-if-empty-dispatched:exclusive=${gate.exclusiveHeld}`);
        keeperDispatched.resolve();
      }
      return 1;
    },
  });

  function rpc(method: string, body: unknown): Promise<Response> {
    return coord.fetch(new Request(`http://test/roost.v1.CoordinatorService/${method}`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: `Bearer ${jwt}`,
      },
      body: JSON.stringify(body),
    }));
  }

  return {
    order,
    gate,
    rpc,
    finalEmptyRecheck: finalEmptyRecheck.promise,
    keeperDispatched: keeperDispatched.promise,
    keeperDispatchCount: () => keeperDispatchCount,
    keeperRequestId: () => keeperRequestId,
    spawnRequestId: (sessionId: string) => spawnRequests.get(sessionId),
    waitForSpawn(sessionId: string): Promise<void> {
      const waiter = Promise.withResolvers<void>();
      spawnWaiters.set(sessionId, waiter);
      return waiter.promise;
    },
    async close(): Promise<void> {
      __setConnectWorkerForTest(WORKER_FP, null);
      rejectPendingRpcsForWorker(WORKER_FP, "keeper fence fixture closed");
      resetPendingSpawnsForTest();
      coord.dispose();
      await opened.close();
      if (existsSync(directory)) rmSync(directory, { recursive: true, force: true });
    },
  };
}

let activeHarness: KeeperFenceHarness | null = null;
afterEach(async () => {
  await activeHarness?.close();
  activeHarness = null;
});

function spawnBody(sessionId: string) {
  return {
    workerFp: WORKER_FP, sessionId, kind: "shell", folder: "/tmp/keeper-fence",
    cols: 80, rows: 24,
  };
}

function resolveSpawn(harness: KeeperFenceHarness, sessionId: string, channelId: number): void {
  const requestId = harness.spawnRequestId(sessionId);
  if (!requestId) throw new Error(`missing spawn request ${sessionId}`);
  harness.order.push(`spawn-result:${sessionId}`);
  expect(resolvePendingRpc(requestId, {
    session_id: sessionId,
    channel_id: channelId,
  }, WORKER_FP)).toBe(true);
}

describe("keeper update admission fence", () => {
  test("drains an admitted spawn before recheck and fences later spawns through success", async () => {
    const harness = await openHarness();
    activeHarness = harness;
    const firstDispatched = harness.waitForSpawn(FIRST_SESSION_ID);
    const firstResponse = harness.rpc("SessionsSpawn", spawnBody(FIRST_SESSION_ID));
    await firstDispatched;
    expect(harness.order).toEqual([
      "normal-acquired:1",
      `spawn-dispatched:${FIRST_SESSION_ID}:normal=1`,
    ]);

    const preparationResponse = harness.rpc("WorkersPrepareKeeperUpdate", {
      workerFp: WORKER_FP, maintenance: true,
    });
    await harness.gate.exclusiveRequested.promise;
    expect(harness.gate.exclusive).toBe(true);
    expect(harness.gate.exclusiveHeld).toBe(false);
    expect(harness.keeperDispatchCount()).toBe(0);
    expect(harness.order).toEqual([
      "normal-acquired:1",
      `spawn-dispatched:${FIRST_SESSION_ID}:normal=1`,
      "exclusive-requested:normal=1",
    ]);

    resolveSpawn(harness, FIRST_SESSION_ID, 41);
    expect((await firstResponse).status).toBe(200);
    await Promise.all([harness.finalEmptyRecheck, harness.keeperDispatched]);
    expect(harness.gate.exclusiveHeld).toBe(true);
    expect(harness.order).toEqual([
      "normal-acquired:1",
      `spawn-dispatched:${FIRST_SESSION_ID}:normal=1`,
      "exclusive-requested:normal=1",
      `spawn-result:${FIRST_SESSION_ID}`,
      "normal-released:0",
      "exclusive-held:normal=0",
      "final-empty-recheck:exclusive=true",
      "shutdown-if-empty-dispatched:exclusive=true",
    ]);

    const blockedResponse = await harness.rpc("SessionsSpawn", spawnBody(BLOCKED_SESSION_ID));
    expect(blockedResponse.status).toBe(503);
    expect(await blockedResponse.json()).toMatchObject({ code: "unavailable" });
    expect(harness.spawnRequestId(BLOCKED_SESSION_ID)).toBeUndefined();
    expect(harness.order.at(-1)).toBe("normal-rejected:exclusive=true");

    const keeperRequestId = harness.keeperRequestId();
    if (!keeperRequestId) throw new Error("missing keeper preparation request");
    harness.order.push("shutdown-if-empty-result");
    expect(resolvePendingRpc(keeperRequestId, { outcome: "shutdown" }, WORKER_FP)).toBe(true);
    const preparation = await preparationResponse;
    expect(preparation.status).toBe(200);
    expect(await preparation.json()).toMatchObject({ outcome: "shutdown" });
    expect(harness.gate.exclusive).toBe(false);
    expect(harness.order.slice(-2)).toEqual([
      "shutdown-if-empty-result",
      "exclusive-released",
    ]);

    const afterDispatched = harness.waitForSpawn(AFTER_SUCCESS_SESSION_ID);
    const afterResponse = harness.rpc("SessionsSpawn", spawnBody(AFTER_SUCCESS_SESSION_ID));
    await afterDispatched;
    resolveSpawn(harness, AFTER_SUCCESS_SESSION_ID, 42);
    expect((await afterResponse).status).toBe(200);
    expect(harness.order.slice(-4)).toEqual([
      "normal-acquired:1",
      `spawn-dispatched:${AFTER_SUCCESS_SESSION_ID}:normal=1`,
      `spawn-result:${AFTER_SUCCESS_SESSION_ID}`,
      "normal-released:0",
    ]);
  });

  test("releases the exclusive lease after a worker preparation failure", async () => {
    const harness = await openHarness();
    activeHarness = harness;
    const preparationResponse = harness.rpc("WorkersPrepareKeeperUpdate", {
      workerFp: WORKER_FP, maintenance: true,
    });
    await Promise.all([
      harness.gate.exclusiveRequested.promise,
      harness.gate.exclusiveAcquired.promise,
      harness.finalEmptyRecheck,
      harness.keeperDispatched,
    ]);
    expect(harness.order).toEqual([
      "exclusive-requested:normal=0",
      "exclusive-held:normal=0",
      "final-empty-recheck:exclusive=true",
      "shutdown-if-empty-dispatched:exclusive=true",
    ]);

    const keeperRequestId = harness.keeperRequestId();
    if (!keeperRequestId) throw new Error("missing keeper preparation request");
    harness.order.push("shutdown-if-empty-error");
    expect(rejectPendingRpc(keeperRequestId, "injected keeper failure", WORKER_FP)).toBe(true);
    const failed = await preparationResponse;
    expect(failed.status).toBe(500);
    expect(await failed.json()).toMatchObject({ code: "internal" });
    expect(harness.gate.exclusive).toBe(false);
    expect(harness.order.slice(-2)).toEqual([
      "shutdown-if-empty-error",
      "exclusive-released",
    ]);

    const spawnDispatched = harness.waitForSpawn(AFTER_FAILURE_SESSION_ID);
    const spawnResponse = harness.rpc("SessionsSpawn", spawnBody(AFTER_FAILURE_SESSION_ID));
    await spawnDispatched;
    resolveSpawn(harness, AFTER_FAILURE_SESSION_ID, 43);
    expect((await spawnResponse).status).toBe(200);
    expect(harness.order.slice(-4)).toEqual([
      "normal-acquired:1",
      `spawn-dispatched:${AFTER_FAILURE_SESSION_ID}:normal=1`,
      `spawn-result:${AFTER_FAILURE_SESSION_ID}`,
      "normal-released:0",
    ]);
  });
});
