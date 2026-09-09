/**
 * Owns Sync WebSocket upgrade authentication, resource-index admission, and revocation contracts.
 * Bun discovers this module directly and gives it an isolated coordinator fixture for mutations.
 * It depends on the real upgrade handler, persisted device identities, and signed JWTs.
 */
import { afterAll, beforeAll, expect, test } from "bun:test";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { UI_TAB_ID_MAX_UTF8_BYTES } from "@roost/shared/ui-state";
import type { ConnectDeps } from "../src/connect/router.ts";
import {
  handleSyncWsUpgrade,
  makeSyncWsHandler,
  type SyncWsData,
} from "../src/connect/sync-ws-handler.ts";
import { invalidateJwtKey, signJwt } from "../src/jwt.ts";
import {
  createSyncWsKeepaliveCoordFixture,
  type SyncWsKeepaliveCoordFixture,
} from "./sync-ws-keepalive-coord-fixture.ts";

let fixture: SyncWsKeepaliveCoordFixture;
let deps: ConnectDeps;
let jwt: string;
let fingerprint: string;

beforeAll(async () => {
  fixture = await createSyncWsKeepaliveCoordFixture();
  ({ deps, jwt, fingerprint } = fixture);
});

afterAll(async () => {
  await fixture?.close();
});

test("rejects missing or malformed auth subprotocols before upgrade", async () => {
  const fakeServer = {
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: () => {
      throw new Error("upgrade must not run");
    },
  };
  for (const protocol of [undefined, "wrong-marker, credential", "roost-auth", "roost-auth,"]) {
    const headers = new Headers();
    if (protocol !== undefined) headers.set("sec-websocket-protocol", protocol);
    const response = await handleSyncWsUpgrade(
      new Request("https://coord.example/ws/coord-sync", { headers }),
      fakeServer,
      deps,
    );
    expect(response?.status, protocol ?? "missing").toBe(401);
  }
});

test("rejects foreign Origin and negotiates roost-auth for an allowed origin", async () => {
  let upgradeHeaders: HeadersInit | undefined;
  const fakeServer = {
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: (_req: Request, opts: { headers?: HeadersInit }) => {
      upgradeHeaders = opts.headers;
      return true;
    },
  };
  const foreign = await handleSyncWsUpgrade(new Request(
    "https://public.example/ws/coord-sync",
    {
      headers: {
        origin: "https://attacker.example",
        "sec-websocket-protocol": `roost-auth, ${jwt}`,
      },
    },
  ), fakeServer, deps);
  expect(foreign?.status).toBe(403);

  const allowed = await handleSyncWsUpgrade(new Request(
    "https://public.example/ws/coord-sync",
    {
      headers: {
        origin: "https://public.example",
        "sec-websocket-protocol": `roost-auth, ${jwt}`,
      },
    },
  ), fakeServer, deps);
  expect(allowed).toBeUndefined();
  expect(new Headers(upgradeHeaders).get("sec-websocket-protocol")).toBe("roost-auth");
});

test("browser Sync bounds a nonblank tab by UTF-8 bytes and preserves unbound semantics", async () => {
  const upgradedData: SyncWsData[] = [];
  const fakeServer = {
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: (_req: Request, options: { data: SyncWsData }) => {
      upgradedData.push(options.data);
      return true;
    },
  };
  const headers = {
    origin: "https://public.example",
    "sec-websocket-protocol": `roost-auth, ${jwt}`,
  };
  const exactTabId = "🙂".repeat(UI_TAB_ID_MAX_UTF8_BYTES / 4);
  for (const tabQuery of [encodeURIComponent(exactTabId), "%20%09"]) {
    expect(await handleSyncWsUpgrade(new Request(
      `https://public.example/ws/coord-sync?flow=1&sync_v=2&tab=${tabQuery}`,
      { headers },
    ), fakeServer, deps)).toBeUndefined();
  }
  const rejected = await handleSyncWsUpgrade(new Request(
    `https://public.example/ws/coord-sync?tab=${encodeURIComponent(`${exactTabId}x`)}`,
    { headers },
  ), fakeServer, deps);
  expect(rejected?.status).toBe(400);
  expect(await rejected?.text()).toBe("connection rejected");
  expect(upgradedData).toHaveLength(2);
  expect(upgradedData[0]?.readOnly).toBe(false);
  expect(upgradedData[0]?.scope.ownerWorkerFp).toBeNull();
  expect(upgradedData[0]?.tabId).toBe(exactTabId);
  expect(upgradedData[0]?.viewerKey).toBe(`${fingerprint}:${exactTabId}`);
  expect(upgradedData[0]?.v2).toBeDefined();
  expect(upgradedData[1]?.tabId).toBeNull();
  expect(upgradedData[1]?.viewerKey).toBeNull();
  expect(deps.uiLayoutApplies.stats().targets).toBe(0);
});

test("worker Sync upgrades read-only over its own resources and cannot bind a viewer", async () => {
  const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const raw = new Uint8Array(await crypto.subtle.exportKey("raw", keys.publicKey));
  const workerFingerprint = await fingerprintOf(raw);
  const { id: dashboardId } = await deps.db.selectFrom("dashboards")
    .select("id")
    .executeTakeFirstOrThrow();
  await deps.db.insertInto("authorized_keys").values({
    fingerprint: workerFingerprint,
    public_key: raw,
    label: "sync-worker",
    added_at: Date.now(),
  }).execute();
  await deps.db.insertInto("workers").values({
    fp: workerFingerprint,
    dashboard_id: dashboardId,
    label: "sync-worker",
    os: "linux",
    registered_at_ms: Date.now(),
    last_seen_ms: Date.now(),
  }).execute();
  const now = Math.floor(Date.now() / 1000);
  const workerJwt = await signJwt(
    { aud: "roost-coordinator", sub: workerFingerprint, iat: now, exp: now + 60 },
    keys.privateKey,
    workerFingerprint,
  );
  let data: SyncWsData | undefined;
  const fakeServer = {
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: (_req: Request, options: { data: SyncWsData }) => {
      data = options.data;
      return true;
    },
  };
  const headers = {
    origin: "https://public.example",
    "sec-websocket-protocol": `roost-auth, ${workerJwt}`,
  };

  expect(await handleSyncWsUpgrade(new Request(
    "https://public.example/ws/coord-sync?tab=forged",
    { headers },
  ), fakeServer, deps)).toBeUndefined();
  expect(data?.scope.ownerWorkerFp).toBe(workerFingerprint);
  expect([...data?.scope.workerFps ?? []]).toEqual([workerFingerprint]);
  expect(data?.readOnly).toBe(true);
  expect(data?.viewerKey).toBeNull();
  expect(data?.tabId).toBeNull();
});

test("only exact flow=1 enables the application window", async () => {
  for (const [query, expected] of [
    ["", false],
    ["?flow=0", false],
    ["?flow=true", false],
    ["?flow=01", false],
    ["?flow=1", true],
  ] as const) {
    const upgradedData: SyncWsData[] = [];
    const fakeServer = {
      requestIP: () => ({ address: "127.0.0.1" }),
      upgrade: (_req: Request, opts: { data: SyncWsData }) => {
        upgradedData.push(opts.data);
        return true;
      },
    };
    const result = await handleSyncWsUpgrade(new Request(
      `https://public.example/ws/coord-sync${query}`,
      {
        headers: {
          origin: "https://public.example",
          "sec-websocket-protocol": `roost-auth, ${jwt}`,
        },
      },
    ), fakeServer, deps);
    expect(result).toBeUndefined();
    expect(upgradedData[0]?.flowControl, query || "absent").toBe(expected);
  }
});

test("revocation between accepted upgrade and open closes before feed registration", async () => {
  let acceptedData: SyncWsData | undefined;
  const fakeServer = {
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: (_req: Request, options: { data: SyncWsData }) => {
      acceptedData = options.data;
      return true;
    },
  };
  const accepted = await handleSyncWsUpgrade(new Request(
    "https://public.example/ws/coord-sync",
    {
      headers: {
        origin: "https://public.example",
        "sec-websocket-protocol": `roost-auth, ${jwt}`,
      },
    },
  ), fakeServer, deps);
  expect(accepted).toBeUndefined();
  if (!acceptedData) throw new Error("upgrade data was not captured");
  invalidateJwtKey(deps.jwtCache, acceptedData.caller.fingerprint);
  let closed: [number, string] | undefined;
  const ws = {
    data: acceptedData,
    close: (code: number, reason: string) => { closed = [code, reason]; },
  };
  makeSyncWsHandler(deps).open(ws as never);
  expect(closed).toEqual([4001, "revoked"]);
  expect(acceptedData.feed).toBeNull();
});
