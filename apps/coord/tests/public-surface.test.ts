import { beforeAll, describe, expect, test } from "bun:test";
import type { CoordConfig } from "@roost/shared/config";
import type { ConnectDeps } from "../src/connect/router.ts";
import type { CoordinatorMoveService } from "../src/coord-move/orchestrator.ts";
import { CoordinatorWriteGate, type CoordinatorWriteMode } from "../src/coord-move/write-gate.ts";
import { makeCfAccessVerifier } from "../src/middleware/cf-access.ts";
import {
  isPublicPathDenied,
  makePublicSurface,
  type PublicServer,
} from "../src/middleware/public-surface.ts";
import {
  ACCESS_AUD,
  ACCESS_TEAM_DOMAIN,
  generateAccessSigningKey,
  signAccessToken,
  staticJwksFetch,
  validAccessClaims,
  type AccessSigningKey,
} from "./helpers/cf-access-fixture.ts";

const NOW_MS = 1_700_000_000_000;
let signingKey: AccessSigningKey;
let accessToken: string;

beforeAll(async () => {
  signingKey = await generateAccessSigningKey("public-surface-key");
  accessToken = await signAccessToken(signingKey, validAccessClaims(NOW_MS / 1000));
});

const cfg: CoordConfig = {
  bind: "127.0.0.1:4103",
  publicBind: "127.0.0.1:4104",
  trustProxy: true,
  dbPath: "/tmp/public-surface.db",
  authorizedKeysPath: "/tmp/authorized_keys",
  webDistPath: undefined,
  coordKeyPath: "/tmp/coord-key",
  jwtMaxAgeSecs: 300,
  auditRetentionDays: 90,
  corsAllowedOrigins: ["https://caller.example"],
  relaxedCsp: false,
  logDir: "/tmp",
  tlsCertPath: undefined,
  tlsKeyPath: undefined,
  publicUrl: "https://private.example.ts.net:4102",
  webPublicUrl: "https://roost.example.com",
  cfAccessTeamDomain: ACCESS_TEAM_DOMAIN,
  cfAccessAud: ACCESS_AUD,
  handoffPath: "/tmp/handoff.json",
};

function moveService(mode: CoordinatorWriteMode = "active"): CoordinatorMoveService {
  return {
    gate: new CoordinatorWriteGate(mode),
    preflight: async () => ({ eligible: false, sourceUrl: "", targetUrl: "", blockers: [] }),
    start: async () => "handoff",
    status: () => null,
    current: () => null,
    recover: async () => {},
    internalStatus: async () => { throw new Error("unused"); },
    internalCommit: async () => {},
    internalAbort: async () => {},
  };
}

const publicServer: PublicServer = {
  requestIP: () => ({ address: "127.0.0.1" }),
  upgrade: () => false,
};

function accessRequest(
  path: string,
  init: RequestInit = {},
  token: string = accessToken,
): Request {
  const headers = new Headers(init.headers);
  headers.set("cf-access-jwt-assertion", token);
  return new Request(`https://roost.example.com${path}`, { ...init, headers });
}

function assertHardened(response: Response): void {
  expect(response.headers.get("strict-transport-security")).toBe("max-age=31536000");
  expect(response.headers.get("x-frame-options")).toBe("DENY");
  expect(response.headers.get("x-content-type-options")).toBe("nosniff");
  expect(response.headers.get("referrer-policy")).toBe("no-referrer");
  expect(response.headers.get("permissions-policy"))
    .toBe("camera=(), geolocation=(), microphone=(self)");
  const csp = response.headers.get("content-security-policy") ?? "";
  expect(csp).toContain("base-uri 'self'");
  expect(csp).toContain("form-action 'none'");
  expect(csp).toContain("object-src 'none'");
  expect(csp).not.toContain("script-src 'self' 'unsafe-inline'");
  expect(csp).not.toContain(" wss: https:");
}

function makeHarness(options: {
  mode?: CoordinatorWriteMode;
  accessReject?: boolean;
  customLimit?: () => Response | null;
  syncResponse?: Response | undefined | null;
} = {}) {
  const coordPaths: string[] = [];
  const syncCalls: Array<{ path: string; reauthAtMs: number | null }> = [];
  const syncDeps = {} as ConnectDeps;
  const access = options.accessReject
    ? { verify: async () => { throw new Error("access rejected"); } }
    : makeCfAccessVerifier(ACCESS_TEAM_DOMAIN, ACCESS_AUD, {
        now: () => NOW_MS,
        fetch: staticJwksFetch(signingKey),
      });
  const surface = makePublicSurface({
    access,
    cfg,
    move: moveService(options.mode),
    syncDeps,
    coord: {
      async fetch(req, ctx) {
        coordPaths.push(new URL(req.url).pathname);
        expect(ctx?.origin.onHost).toBe(false);
        expect(ctx?.origin.clientIp).toBe("203.0.113.9");
        expect(ctx?.dbExport).toBeUndefined();
        return new Response("coord", { status: 200 });
      },
    },
    spa: () => new Response("spa"),
    now: () => NOW_MS,
    customLimit: options.customLimit
      ? () => options.customLimit!()
      : () => null,
    syncUpgrade: async (req, _server, _deps, reauthAtMs) => {
      syncCalls.push({ path: new URL(req.url).pathname, reauthAtMs: reauthAtMs ?? null });
      return options.syncResponse;
    },
  });
  return { surface, coordPaths, syncCalls };
}
function publicRequest(
  path: string,
  init: RequestInit = {},
  token: string = accessToken,
): Request {
  const req = accessRequest(path, init, token);
  req.headers.set("cf-connecting-ip", "203.0.113.9");
  return req;
}

describe("public path policy", () => {
  const denied = [
    "/internal/coord-handoff/commit",
    `/ws/coord-worker/${"a".repeat(64)}`,
    "/api/db-export",
    "/roost.v1.CoordinatorService/AuthAuthorizeBrowser",
    "/roost.v1.CoordinatorService/AuthRedeemWorker",
    "/roost.v1.CoordinatorService/AuthMintCoordinatorRelocation",
    "/roost.v1.CoordinatorService/AuthRedeemCoordinatorRelocation",
    "/roost.v1.CoordinatorService/CoordinatorMovePreflight",
    "/roost.v1.CoordinatorService/CoordinatorMoveStart",
    "/roost.v1.CoordinatorService/CoordinatorMoveStatus",
    "/roost.v1.CoordinatorService/MiscDbExportUrl",
  ];

  test("every catastrophic path is denied after Access without reaching a sink", async () => {
    const { surface, coordPaths, syncCalls } = makeHarness();
    for (const path of denied) {
      expect(isPublicPathDenied(path)).toBe(true);
      const response = await surface.fetch(publicRequest(path, { method: "POST" }), publicServer);
      expect(response?.status, path).toBe(404);
    }
    expect(coordPaths).toEqual([]);
    expect(syncCalls).toEqual([]);
  });

  test("Access authentication precedes policy classification", async () => {
    const { surface } = makeHarness({ accessReject: true });
    for (const path of ["/", "/api/db-export", "/roost.v1.CoordinatorService/AuthAuthorizeBrowser"]) {
      const response = await surface.fetch(new Request(`https://roost.example.com${path}`), publicServer);
      expect(response?.status).toBe(401);
      expect(response?.headers.get("x-roost-auth-layer")).toBe("access");
      if (response) assertHardened(response);
    }
  });

  test("allowed RPCs and SPA paths delegate only to coord", async () => {
    const paths = [
      "/",
      "/roost.v1.CoordinatorService/AuthCoordIdentity",
      "/roost.v1.CoordinatorService/AuthMintBootstrap",
      "/roost.v1.CoordinatorService/AuthRedeemBrowser",
      "/roost.v1.CoordinatorService/PairCreate",
      "/roost.v1.CoordinatorService/PairPoll",
      "/roost.v1.CoordinatorService/PairList",
      "/roost.v1.CoordinatorService/PairApprove",
      "/roost.v1.CoordinatorService/PairDeny",
      "/roost.v1.CoordinatorService/DevicesList",
      "/roost.v1.CoordinatorService/DevicesRevoke",
      "/roost.v1.CoordinatorService/DevicesRotateCurrent",
      "/roost.v1.CoordinatorService/MiscHealth",
      "/roost.v1.CoordinatorService/SessionsInput",
    ];
    const { surface, coordPaths, syncCalls } = makeHarness();
    for (const path of paths) {
      const response = await surface.fetch(publicRequest(path, { method: "POST" }), publicServer);
      expect(response?.status, path).toBe(200);
    }
    expect(coordPaths).toEqual(paths);
    expect(syncCalls).toEqual([]);
  });

  test("Sync reaches only its upgrade sink with an Access-bounded deadline", async () => {
    const { surface, coordPaths, syncCalls } = makeHarness({ syncResponse: undefined });
    const response = await surface.fetch(publicRequest("/ws/coord-sync"), publicServer);
    expect(response).toBeUndefined();
    expect(coordPaths).toEqual([]);
    expect(syncCalls).toEqual([{ path: "/ws/coord-sync", reauthAtMs: NOW_MS + 300_000 }]);
  });

  test("Sync deadline uses an earlier Access expiry", async () => {
    const claims = validAccessClaims(NOW_MS / 1000);
    claims.exp = NOW_MS / 1000 + 10;
    const shortToken = await signAccessToken(signingKey, claims);
    const { surface, syncCalls } = makeHarness({ syncResponse: undefined });
    const response = await surface.fetch(
      publicRequest("/ws/coord-sync", {}, shortToken),
      publicServer,
    );
    expect(response).toBeUndefined();
    expect(syncCalls).toEqual([{
      path: "/ws/coord-sync",
      reauthAtMs: NOW_MS + 10_000,
    }]);
  });
});

describe("public move gate and response finalization", () => {
  test("retired mode allows discovery but rejects writes", async () => {
    const { surface, coordPaths } = makeHarness({ mode: "retired" });
    expect((await surface.fetch(publicRequest(
      "/roost.v1.CoordinatorService/AuthCoordIdentity",
      { method: "POST" },
    ), publicServer))?.status).toBe(200);
    expect((await surface.fetch(publicRequest(
      "/roost.v1.CoordinatorService/MiscHealth",
      { method: "POST" },
    ), publicServer))?.status).toBe(200);
    expect((await surface.fetch(publicRequest(
      "/roost.v1.CoordinatorService/SessionsInput",
      { method: "POST" },
    ), publicServer))?.status).toBe(410);
    expect(coordPaths).toEqual([
      "/roost.v1.CoordinatorService/AuthCoordIdentity",
      "/roost.v1.CoordinatorService/MiscHealth",
    ]);
  });

  test("hardens 404, 429, Sync rejection, delegated response, and 500", async () => {
    const deniedHarness = makeHarness();
    const denied = await deniedHarness.surface.fetch(publicRequest("/api/db-export"), publicServer);
    expect(denied?.status).toBe(404);
    if (denied) assertHardened(denied);

    const limitedHarness = makeHarness({
      accessReject: true,
      customLimit: () => new Response("limited", { status: 429 }),
    });
    const limited = await limitedHarness.surface.fetch(new Request("https://roost.example.com/"), publicServer);
    expect(limited?.status).toBe(429);
    if (limited) assertHardened(limited);

    const syncHarness = makeHarness({ syncResponse: new Response("bad sync", { status: 403 }) });
    const syncRejected = await syncHarness.surface.fetch(publicRequest("/ws/coord-sync"), publicServer);
    expect(syncRejected?.status).toBe(403);
    if (syncRejected) assertHardened(syncRejected);

    const delegated = await deniedHarness.surface.fetch(publicRequest("/"), publicServer);
    if (delegated) assertHardened(delegated);

    const failure = deniedHarness.surface.error(new Error("boom"));
    expect(failure.status).toBe(500);
    assertHardened(failure);
  });

  test("exposes auth provenance only to explicitly allowed CORS origins", async () => {
    const { surface } = makeHarness({ accessReject: true });
    const response = await surface.fetch(new Request("https://roost.example.com/", {
      headers: { origin: "https://caller.example" },
    }), publicServer);
    expect(response?.headers.get("access-control-allow-origin")).toBe("https://caller.example");
    expect(response?.headers.get("access-control-expose-headers")).toContain("x-roost-auth-layer");
  });
});
