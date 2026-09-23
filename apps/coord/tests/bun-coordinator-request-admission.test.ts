/**
 * Covers loopback Host and Origin admission at the Bun coordinator listener.
 * The captured serve seam observes real listener responses without starting a network socket.
 * It proves rejected traffic cannot reach coordinator routes or raw WebSocket upgrades.
 */

import { describe, expect, test } from "bun:test";
import { DEFAULT_WORKER_LOCAL_UI_ORIGIN, type CoordConfig } from "@roost/shared/config";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import { SYNC_WS_PATH } from "@roost/shared/wire/sync-ws";
import { startBunCoordinatorListeners } from "../src/bun-coordinator-listeners.ts";
import {
  createCoordinatorRequestAdmission,
  type CoordinatorRequestAdmission,
} from "../src/middleware/coordinator-request-admission.ts";

const BOUND_PORT = 43123;
const LOCAL_AUTHORITY = `127.0.0.1:${BOUND_PORT}`;
const LOCAL_ORIGIN = `http://${LOCAL_AUTHORITY}`;
const WORKER_SOCKET_PATH = `/ws/coord-worker/${"a".repeat(64)}`;
const PAIR_CREATE_PATH =
  `/${CoordinatorService.typeName}/${CoordinatorService.method.pairCreate.name}`;

interface ListenerServer {
  readonly port: number;
  requestIP(request: Request): { address: string } | null;
  upgrade(request: Request, options: unknown): boolean;
  timeout(request: Request, seconds: number): void;
}

interface CapturedServeOptions {
  fetch?: (
    request: Request,
    server: ListenerServer,
  ) => Response | undefined | Promise<Response | undefined>;
}

interface ListenerFixture {
  dispatch(request: Request): Promise<Response>;
  readonly coordinatorFetches: number;
  readonly upgrades: number;
}

function coordinatorConfig(bind = "127.0.0.1:0"): CoordConfig {
  return {
    bind,
    dbPath: "/tmp/roost-coordinator-request-admission-missing/coordinator.db",
    authorizedKeysPath: "/tmp/coordinator-request-admission.keys",
    webDistPath: undefined,
    jwtMaxAgeSecs: 300,
    auditRetentionDays: 90,
    corsAllowedOrigins: ["https://cors.example"],
    pushAllowedOrigins: [],
    relaxedCsp: false,
    trustProxy: false,
    logDir: "/tmp",
    webPublicUrl: "https://dashboard.example:443",
    publicUrl: "https://worker-door.example:4443",
    terminalPeerEnabled: false,
    terminalPeerStunUrls: [],
  };
}

function requestFor(
  path: string,
  headers: HeadersInit = {},
  method = "GET",
): Request {
  return new Request(`${LOCAL_ORIGIN}${path}`, { method, headers });
}

async function expectForbidden(
  admission: CoordinatorRequestAdmission,
  request: Request,
  kind: "host" | "origin",
): Promise<void> {
  const response = admission(request);
  if (!response) throw new Error("request unexpectedly admitted");
  expect(response.status).toBe(403);
  expect(await response.text()).toBe(`forbidden ${kind}`);
}

function makeListenerFixture(): ListenerFixture {
  const serveOptions: CapturedServeOptions[] = [];
  let coordinatorFetches = 0;
  let upgrades = 0;
  const server: ListenerServer = {
    port: BOUND_PORT,
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: () => {
      upgrades += 1;
      return true;
    },
    timeout(): void {},
  };
  // The listener's Bun-only transport dependencies are inert because this
  // fixture drives only gate-rejected upgrades and a portable fetch stub.
  const serve = ((options: unknown) => {
    serveOptions.push(options as CapturedServeOptions);
    return server;
  }) as unknown as typeof Bun.serve;
  const listenerDeps = {
    cfg: coordinatorConfig(),
    coord: {
      async fetch(): Promise<Response> {
        coordinatorFetches += 1;
        return new Response("coordinator reached");
      },
      dispose(): void {},
    },
    sqlite: {},
    workerDeps: {},
    syncDeps: {},
    workerWs: { open() {}, message() {}, close() {} },
    syncWs: { open() {}, message() {}, drain() {}, close() {} },
    spa: () => new Response("spa"),
    _serve: serve,
  };
  startBunCoordinatorListeners(
    listenerDeps as unknown as Parameters<typeof startBunCoordinatorListeners>[0],
  );

  const listenerFetch = serveOptions[0]?.fetch;
  if (!listenerFetch) throw new Error("listener was never constructed");
  return {
    async dispatch(request): Promise<Response> {
      const response = await listenerFetch(request, server);
      if (!response) throw new Error("request unexpectedly upgraded");
      return response;
    },
    get coordinatorFetches(): number {
      return coordinatorFetches;
    },
    get upgrades(): number {
      return upgrades;
    },
  };
}

describe("coordinator loopback request admission", () => {
  test("allows only canonical or declared Hosts and exact admitted Origins", async () => {
    const admission = createCoordinatorRequestAdmission(coordinatorConfig(), BOUND_PORT);

    for (const host of [LOCAL_AUTHORITY, "dashboard.example", "dashboard.example:443", "worker-door.example:4443"]) {
      expect(admission(requestFor("/", { host }))).toBeNull();
    }
    for (const origin of [
      LOCAL_ORIGIN,
      "https://dashboard.example",
      "https://worker-door.example:4443",
      DEFAULT_WORKER_LOCAL_UI_ORIGIN,
      "https://cors.example",
    ]) {
      expect(admission(requestFor("/", { host: LOCAL_AUTHORITY, origin }))).toBeNull();
    }
    expect(admission(requestFor("/", { host: LOCAL_AUTHORITY }))).toBeNull();

    await expectForbidden(admission, requestFor("/"), "host");
    await expectForbidden(admission, requestFor("/", { host: `${LOCAL_AUTHORITY}/` }), "host");
    await expectForbidden(admission, requestFor("/", { host: "cors.example" }), "host");
    for (const origin of [
      `http://127.0.0.1:${BOUND_PORT + 1}`,
      `http://localhost:${BOUND_PORT}`,
      `http://192.168.1.20:${BOUND_PORT}`,
      "https://attacker.example",
      "null",
    ]) {
      await expectForbidden(
        admission,
        requestFor("/", { host: LOCAL_AUTHORITY, origin }),
        "origin",
      );
    }
  });

  test("leaves non-loopback developer listeners unchanged", () => {
    const admission = createCoordinatorRequestAdmission(coordinatorConfig("0.0.0.0:4103"), BOUND_PORT);
    expect(admission(requestFor("/", { host: "attacker.example", origin: "null" }))).toBeNull();
  });

  test("rejects Host and Origin attacks before coordinator routes or upgrades", async () => {
    const fixture = makeListenerFixture();
    for (const [path, method] of [
      ["/", "GET"],
      ["/api/db-export", "GET"],
      [PAIR_CREATE_PATH, "POST"],
      [WORKER_SOCKET_PATH, "GET"],
      [SYNC_WS_PATH, "GET"],
      ["/roost.v1.CoordinatorService/Sync", "POST"],
    ] as const) {
      const response = await fixture.dispatch(requestFor(path, { host: "attacker.example" }, method));
      expect(response.status).toBe(403);
      expect(await response.text()).toBe("forbidden host");
    }
    const foreignOrigin = await fixture.dispatch(requestFor("/api/db-export", {
      host: LOCAL_AUTHORITY,
      origin: "https://attacker.example",
    }));
    expect(foreignOrigin.status).toBe(403);
    expect(await foreignOrigin.text()).toBe("forbidden origin");
    expect(fixture.coordinatorFetches).toBe(0);
    expect(fixture.upgrades).toBe(0);
  });

  test("passes canonical browser, declared HTTPS, no-Origin CLI, and worker traffic to handlers", async () => {
    const fixture = makeListenerFixture();
    const localBrowserResponse = await fixture.dispatch(requestFor("/", {
      host: LOCAL_AUTHORITY,
      origin: LOCAL_ORIGIN,
    }));
    expect(localBrowserResponse.status).toBe(200);

    const declaredBrowserResponse = await fixture.dispatch(requestFor("/", {
      host: "dashboard.example",
      origin: "https://dashboard.example",
    }));
    expect(declaredBrowserResponse.status).toBe(200);

    const cliResponse = await fixture.dispatch(requestFor("/", { host: LOCAL_AUTHORITY }));
    expect(cliResponse.status).toBe(200);
    expect(fixture.coordinatorFetches).toBe(3);

    const workerResponse = await fixture.dispatch(requestFor(WORKER_SOCKET_PATH, {
      host: LOCAL_AUTHORITY,
    }));
    expect(workerResponse.status).toBe(401);
    expect(await workerResponse.text()).toBe("unauthorized");
    expect(fixture.upgrades).toBe(0);
  });
});
