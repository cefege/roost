// The internet-facing listener surface has two boot-selected policies:
// complete Cloudflare Access configuration preserves the legacy Access gate;
// managed mode without Access uses an explicit, default-deny route allowlist.
// Both apply availability gating, the coord-sync WS upgrade, and hardened
// response headers before coordinator dispatch.
import type { CoordConfig } from "@roost/shared/config";
import { signal as emitSignal } from "@roost/shared/diag";
import type { ConnectDeps } from "../connect/router.ts";
import {
  handleSyncWsUpgrade,
  type SyncUpgradeServer,
} from "../connect/sync-ws-handler.ts";
import {
  handleWorkerWsUpgrade,
  type WorkerWsData,
} from "../connect/worker-ws-handler.ts";
import type { WorkerServiceDeps } from "../connect/worker-service.ts";
import type { Server } from "bun";
import type { CoordinatorMoveService } from "../coord-move/orchestrator.ts";
import type { CoordHandle } from "../coord-factory.ts";
import { resolveCallerOrigin } from "./caller-origin.ts";
import type { CfAccessIdentity, CfAccessVerifier } from "./cf-access.ts";
import {
  AUTH_LAYER_ACCESS,
  AUTH_LAYER_DEVICE,
  X_ROOST_AUTH_LAYER,
} from "@roost/shared/wire/headers";
import { SYNC_WS_PATH } from "@roost/shared/wire/sync-ws";
import { coordinatorAvailabilityResponse } from "./coordinator-availability.ts";
import { checkCustomLimit } from "./rate-limit.ts";
import {
  applyCors,
  applySecurityHeaders,
  securityOptionsForConfig,
  type SecurityOptions,
} from "./security.ts";
import {
  classifyManagedPublicRoute,
  COORDINATOR_RPC_PREFIX,
  isPublicPathDenied,
  MANAGED_CREDENTIAL_LIMITS,
  type ManagedPublicRouteKind,
} from "./managed-route-policy.ts";

export { classifyManagedPublicRoute, isPublicPathDenied };
export type { ManagedPublicRouteKind };

export interface PublicServer extends SyncUpgradeServer {}

export interface PublicSurfaceDeps {
  access?: CfAccessVerifier | undefined;
  coord: Pick<CoordHandle, "fetch">;
  syncUpgrade?: typeof handleSyncWsUpgrade;
  workerUpgrade?: (
    req: Request,
    server: PublicServer,
    deps: WorkerServiceDeps,
  ) => Promise<Response | undefined | null>;
  workerDeps: WorkerServiceDeps;
  syncDeps: ConnectDeps;
  move: CoordinatorMoveService;
  cfg: CoordConfig;
  spa: (url: URL, method: string, acceptEncoding: string) => Promise<Response> | Response;
  now?: () => number;
  customLimit?: (
    key: string,
    group: string,
    tokensPerWindow: number,
    windowMs?: number,
  ) => Response | null;
  signal?: (event: string, fields: Record<string, unknown>) => void;
}

export function finalizePublicResponse(
  response: Response,
  security: SecurityOptions,
  req?: Request,
): Response {
  const headers = new Headers(response.headers);
  if (req) applyCors(headers, req.headers.get("origin"), security.corsAllowedOrigins);
  applySecurityHeaders(
    headers,
    security.relaxedCsp,
    security.hsts,
    security.connectOrigins,
    security.managed,
  );
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers,
  });
}

export function makePublicSurface(deps: PublicSurfaceDeps): {
  fetch(req: Request, server: PublicServer): Promise<Response | undefined>;
  error(err: Error): Response;
} {
  const now = deps.now ?? Date.now;
  const customLimit = deps.customLimit ?? checkCustomLimit;
  const report = deps.signal ?? emitSignal;
  const syncUpgrade = deps.syncUpgrade ?? handleSyncWsUpgrade;
  const workerUpgrade = deps.workerUpgrade
    ?? ((req, server, workerDeps) => handleWorkerWsUpgrade(
      req,
      server as unknown as Server<WorkerWsData>,
      workerDeps,
    ));
  const security = securityOptionsForConfig(deps.cfg, true);

  async function fetch(req: Request, server: PublicServer): Promise<Response | undefined> {
    // The edge address is attribution/rate-limit input only. public-edge
    // resolution rejects loopback/tailnet claims and always keeps onHost false,
    // so a forged proxy header can never cross a trust-gated handler boundary.
    // Keeping distinct public addresses avoids one global SaaS rate-limit key.
    const origin = resolveCallerOrigin(
      "public-edge",
      server.requestIP(req)?.address,
      req.headers,
    );

    let accessIdentity: CfAccessIdentity | undefined;
    if (!deps.cfg.saasMode && deps.access) {
      try {
        accessIdentity = await deps.access.verify(req);
      } catch (error) {
        report("auth.rpc_rejected", {
          reason: error instanceof Error ? error.message : String(error),
          addr: origin.clientIp,
          cooldownKey: origin.clientIp,
        });
        const limited = customLimit(origin.clientIp, "public-access-auth", 100);
        if (limited) return finalizePublicResponse(limited, security, req);
        return finalizePublicResponse(new Response("unauthorized", {
          status: 401,
          headers: { [X_ROOST_AUTH_LAYER]: AUTH_LAYER_ACCESS },
        }), security, req);
      }
    }

    const url = new URL(req.url);
    let route: ManagedPublicRouteKind | "access-delegated";
    if (!deps.cfg.saasMode) {
      if (!deps.access) {
        return finalizePublicResponse(new Response("not found", { status: 404 }), security, req);
      }
      if (isPublicPathDenied(url.pathname)) {
        return finalizePublicResponse(new Response("not found", { status: 404 }), security, req);
      }
      route = url.pathname === SYNC_WS_PATH ? "sync" : "access-delegated";
    } else {
      route = classifyManagedPublicRoute(url.pathname, req.method);
      if (route === "denied") {
        return finalizePublicResponse(new Response("not found", { status: 404 }), security, req);
      }
    }
    if (deps.cfg.saasMode && route !== "spa") {
      const baseLimited = customLimit(origin.clientIp, "managed-public-base", 100);
      if (baseLimited) return finalizePublicResponse(baseLimited, security, req);

      if (req.method === "POST" && url.pathname.startsWith(COORDINATOR_RPC_PREFIX)) {
        const rpcMethod = url.pathname.slice(COORDINATOR_RPC_PREFIX.length);
        const credentialLimit = MANAGED_CREDENTIAL_LIMITS[rpcMethod];
        if (credentialLimit) {
          const limited = customLimit(
            origin.clientIp,
            credentialLimit.group,
            credentialLimit.tokensPerWindow,
          );
          if (limited) return finalizePublicResponse(limited, security, req);
        }
      }
    }


    if ((route === "protected-rpc" || route === "worker-rpc") && req.method !== "OPTIONS") {
      const authorization = req.headers.get("authorization");
      const hasBearer = authorization?.startsWith("Bearer ")
        && authorization.slice("Bearer ".length).trim().length > 0;
      if (!hasBearer) {
        report("auth.rpc_rejected", {
          path: url.pathname,
          reason: "no_device_bearer",
          addr: origin.clientIp,
          cooldownKey: origin.clientIp,
        });
        const limited = customLimit(origin.clientIp, "public-device-auth", 100);
        if (limited) return finalizePublicResponse(limited, security, req);
        return finalizePublicResponse(new Response("unauthorized", {
          status: 401,
          headers: { [X_ROOST_AUTH_LAYER]: AUTH_LAYER_DEVICE },
        }), security, req);
      }
    }

    const unavailable = coordinatorAvailabilityResponse(
      deps.move.gate.mode,
      req.method,
      url.pathname,
    );
    if (unavailable) return finalizePublicResponse(unavailable, security, req);

    if (route === "sync") {
      const limited = customLimit(origin.clientIp, "public-sync-upgrade", 100);
      if (limited) return finalizePublicResponse(limited, security, req);
      const reauthAtMs = accessIdentity
        ? Math.min(accessIdentity.exp * 1000, now() + 300_000)
        : null;
      const upgraded = await syncUpgrade(req, server, deps.syncDeps, reauthAtMs);
      if (upgraded === undefined) return undefined;
      return finalizePublicResponse(
        upgraded ?? new Response("not found", { status: 404 }),
        security,
        req,
      );
    }

    if (route === "worker") {
      const limited = customLimit(origin.clientIp, "public-worker-upgrade", 100);
      if (limited) return finalizePublicResponse(limited, security, req);
      const upgraded = await workerUpgrade(req, server, deps.workerDeps);
      if (upgraded === undefined) return undefined;
      return finalizePublicResponse(
        upgraded ?? new Response("not found", { status: 404 }),
        security,
        req,
      );
    }

    if (route === "worker-redeem-rpc" && req.method === "POST") {
      const limited = customLimit(origin.clientIp, "public-worker-redeem", 20);
      if (limited) return finalizePublicResponse(limited, security, req);
    }

    const response = await deps.coord.fetch(req, {
      origin,
      spa: deps.spa,
      hsts: true,
    });
    return finalizePublicResponse(response, security, req);
  }

  function error(_err: Error): Response {
    return finalizePublicResponse(new Response(
      JSON.stringify({ error: "internal server error" }),
      { status: 500, headers: { "content-type": "application/json" } },
    ), security);
  }

  return { fetch, error };
}
