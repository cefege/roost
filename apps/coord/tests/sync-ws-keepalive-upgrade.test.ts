/**
 * Owns Sync WebSocket upgrade authentication, dashboard scope, and revocation contracts.
 * Bun discovers this module directly and gives it an isolated coordinator fixture for mutations.
 * It depends on the real upgrade handler, persisted device identities, and signed JWTs.
 */
import { afterAll, beforeAll, expect, test } from "bun:test";
import { fingerprintOf } from "@roost/shared/fingerprint";
import type { ConnectDeps } from "../src/connect/router.ts";
import {
  handleSyncWsUpgrade,
  makeSyncWsHandler,
  type SyncWsData,
} from "../src/connect/sync-ws-handler.ts";
import { invalidateJwtKey, signJwt } from "../src/jwt.ts";
import {
  createSyncWsKeepaliveCoordFixture,
  SYNC_WS_KEEPALIVE_DASHBOARD_ID,
  type SyncWsKeepaliveCoordFixture,
} from "./sync-ws-keepalive-coord-fixture.ts";

let fixture: SyncWsKeepaliveCoordFixture;
let deps: ConnectDeps;
let jwt: string;
let fingerprint: string;
const dashboardId = SYNC_WS_KEEPALIVE_DASHBOARD_ID;

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
    `https://public.example/ws/coord-sync?dashboard=${dashboardId}`,
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

test("rejects an unavailable selected dashboard before socket upgrade", async () => {
  const result = await handleSyncWsUpgrade(new Request(
    "https://public.example/ws/coord-sync?dashboard=foreign-dashboard",
    {
      headers: {
        origin: "https://public.example",
        "sec-websocket-protocol": `roost-auth, ${jwt}`,
      },
    },
  ), {
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: () => {
      throw new Error("foreign dashboard must not upgrade");
    },
  }, deps);
  expect(result?.status).toBe(404);
});

test("worker Sync derives a read-only scope from its persisted dashboard", async () => {
  const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const raw = new Uint8Array(await crypto.subtle.exportKey("raw", keys.publicKey));
  const workerFingerprint = await fingerprintOf(raw);
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

  expect((await handleSyncWsUpgrade(new Request(
    "https://public.example/ws/coord-sync",
    { headers },
  ), fakeServer, deps))?.status).toBe(404);
  expect((await handleSyncWsUpgrade(new Request(
    "https://public.example/ws/coord-sync?dashboard=foreign",
    { headers },
  ), fakeServer, deps))?.status).toBe(404);
  expect(await handleSyncWsUpgrade(new Request(
    `https://public.example/ws/coord-sync?dashboard=${dashboardId}&tab=forged`,
    { headers },
  ), fakeServer, deps)).toBeUndefined();
  expect(data?.actor.dashboardId).toBe(dashboardId);
  expect(data?.readOnly).toBe(true);
  expect(data?.viewerKey).toBeNull();
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
      `https://public.example/ws/coord-sync?dashboard=${dashboardId}${query ? `&${query.slice(1)}` : ""}`,
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
    `https://public.example/ws/coord-sync?dashboard=${dashboardId}`,
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
