/**
 * WHY: This fixture owns the deterministic public-edge harness shared by public-surface suites.
 * Discovered tests call it to construct middleware requests and inspect delegated calls.
 * It depends on the real public-surface, move gate, and Cloudflare Access verifier boundaries.
 */
import { expect } from "bun:test";
import type { CoordConfig } from "@roost/shared/config";
import type { ConnectDeps } from "../src/connect/router.ts";
import type { WorkerServiceDeps } from "../src/connect/worker-service.ts";
import type { CoordinatorMoveService } from "../src/coord-move/orchestrator.ts";
import {
  CoordinatorWriteGate,
  type CoordinatorWriteMode,
} from "../src/coord-move/write-gate.ts";
import { makeCfAccessVerifier } from "../src/middleware/cf-access.ts";
import {
  makePublicSurface,
  type PublicServer,
} from "../src/middleware/public-surface.ts";
import {
  ACCESS_AUD,
  ACCESS_TEAM_DOMAIN,
  staticJwksFetch,
  type AccessSigningKey,
} from "./helpers/cf-access-fixture.ts";

export const NOW_MS = 1_700_000_000_000;
export const RPC = "/roost.v1.CoordinatorService/";

const managedCfg: CoordConfig = {
  bind: "127.0.0.1:4103",
  publicBind: "127.0.0.1:4104",
  trustProxy: true,
  saasMode: true,
  managedContainer: true,
  dbPath: "/tmp/public-surface.db",
  authorizedKeysPath: "/tmp/authorized_keys",
  webDistPath: undefined,
  coordKeyPath: "/tmp/coord-key",
  jwtMaxAgeSecs: 300,
  auditRetentionDays: 90,
  corsAllowedOrigins: ["https://caller.example"],
  pushAllowedOrigins: [],
  relaxedCsp: false,
  logDir: "/tmp",
  tlsCertPath: undefined,
  tlsKeyPath: undefined,
  publicUrl: "https://private.example.ts.net:4102",
  webPublicUrl: "https://roost.example.com",
  cfAccessTeamDomain: undefined,
  cfAccessAud: undefined,
  handoffPath: "/tmp/handoff.json",
};

function moveService(mode: CoordinatorWriteMode = "active"): CoordinatorMoveService {
  return {
    gate: new CoordinatorWriteGate(mode),
    preflight: async () => ({ eligible: false, sourceUrl: "", targetUrl: "", blockers: [] }),
    start: async () => "handoff",
    statusForWorker: async () => null,
    status: () => null,
    current: () => null,
    recover: async () => {},
    internalStatus: async () => { throw new Error("unused"); },
    internalCommit: async () => {},
    internalAbort: async () => {},
  };
}

export const publicServer: PublicServer = {
  requestIP: () => ({ address: "127.0.0.1" }),
  upgrade: () => false,
};

interface CoordCall {
  path: string;
  authorization: string | null;
  origin: {
    listener: string;
    clientIp: string;
    onHost: boolean;
  };
}

interface HarnessControls {
  mode?: CoordinatorWriteMode;
  customLimit?: () => Response | null;
  syncResponse?: Response | undefined | null;
  workerResponse?: Response | undefined | null;
}

type HarnessOptions =
  | (HarnessControls & {
      access: "valid";
      accessSigningKey: AccessSigningKey;
    })
  | (HarnessControls & {
      access?: "reject";
      accessSigningKey?: never;
    });

export function makeHarness(options: HarnessOptions = {}) {
  const coordCalls: CoordCall[] = [];
  const syncCalls: Array<{ path: string; reauthAtMs: number | null }> = [];
  const workerCalls: string[] = [];
  const rateGroups: string[] = [];
  const rateCalls: Array<{
    clientIp: string;
    group: string;
    tokensPerWindow: number;
    windowMs: number | undefined;
  }> = [];
  const syncDeps = {} as ConnectDeps;
  const workerDeps = {} as WorkerServiceDeps;
  const access = options.access === "valid"
    ? makeCfAccessVerifier(ACCESS_TEAM_DOMAIN, ACCESS_AUD, {
        now: () => NOW_MS,
        fetch: staticJwksFetch(options.accessSigningKey),
      })
    : options.access === "reject"
    ? { verify: async () => { throw new Error("access rejected"); } }
    : undefined;
  const surface = makePublicSurface({
    access,
    cfg: options.access
      ? {
          ...managedCfg,
          saasMode: false,
          cfAccessTeamDomain: ACCESS_TEAM_DOMAIN,
          cfAccessAud: ACCESS_AUD,
        }
      : managedCfg,
    move: moveService(options.mode),
    syncDeps,
    workerDeps,
    coord: {
      async fetch(req, ctx) {
        if (!ctx) throw new Error("public surface omitted coordinator context");
        expect(ctx.dbExport).toBeUndefined();
        coordCalls.push({
          path: new URL(req.url).pathname,
          authorization: req.headers.get("authorization"),
          origin: ctx.origin,
        });
        return new Response("coord", { status: 200 });
      },
    },
    spa: () => new Response("spa"),
    now: () => NOW_MS,
    customLimit: (clientIp, group, tokensPerWindow, windowMs) => {
      rateGroups.push(`${clientIp}:${group}`);
      rateCalls.push({ clientIp, group, tokensPerWindow, windowMs });
      return options.customLimit?.() ?? null;
    },
    syncUpgrade: async (req, _server, _deps, reauthAtMs) => {
      syncCalls.push({
        path: new URL(req.url).pathname,
        reauthAtMs: reauthAtMs ?? null,
      });
      return options.syncResponse;
    },
    workerUpgrade: async (req) => {
      workerCalls.push(new URL(req.url).pathname);
      return options.workerResponse;
    },
  });
  return { surface, coordCalls, syncCalls, workerCalls, rateGroups, rateCalls };
}

export function request(path: string, init: RequestInit = {}): Request {
  return new Request(`https://roost.example.com${path}`, init);
}

export function managedRequest(path: string, init: RequestInit = {}): Request {
  const headers = new Headers(init.headers);
  headers.set("cf-connecting-ip", "203.0.113.9");
  headers.set("x-forwarded-for", "100.100.100.100");
  headers.set("x-roost-remote-addr", "127.0.0.1");
  headers.set("x-roost-on-host", "1");
  headers.set("x-roost-listener-trust", "tailscale-serve");
  return request(path, { ...init, headers });
}

export function deviceRequest(path: string, init: RequestInit = {}): Request {
  const req = managedRequest(path, init);
  req.headers.set("authorization", "Bearer device-jwt");
  return req;
}

export function assertHardened(response: Response): void {
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
