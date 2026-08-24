// The internet-facing listener surface: CF Access verification, public deny
// lists, availability gating, the coord-sync WS upgrade, then coord dispatch.
// Order is load-bearing — access.verify runs before any routing so
// unauthenticated traffic never reaches RPC handling. PUBLIC_DENIED_* must
// name every privileged RPC/path: this listener has no auth layer beyond
// the Access token, so an unlisted privileged route is world-reachable.
import type { CoordConfig } from "@roost/shared/config";
import { signal as emitSignal } from "@roost/shared/diag";
import type { ConnectDeps } from "../connect/router.ts";
import {
  handleSyncWsUpgrade,
  type SyncUpgradeServer,
} from "../connect/sync-ws-handler.ts";
import type { CoordinatorMoveService } from "../coord-move/orchestrator.ts";
import type { CoordHandle } from "../coord-factory.ts";
import { resolveCallerOrigin } from "./caller-origin.ts";
import type { CfAccessVerifier } from "./cf-access.ts";
import { coordinatorAvailabilityResponse } from "./coordinator-availability.ts";
import { checkCustomLimit } from "./rate-limit.ts";
import {
  applyCors,
  applySecurityHeaders,
  securityOptionsForConfig,
  type SecurityOptions,
} from "./security.ts";

const PUBLIC_DENIED_PREFIXES = [
  "/internal/",
  "/ws/coord-worker/",
  "/api/db-export",
];

const PUBLIC_DENIED_RPCS: ReadonlySet<string> = new Set([
  "/roost.v1.CoordinatorService/AuthAuthorizeBrowser",
  "/roost.v1.CoordinatorService/AuthRedeemWorker",
  "/roost.v1.CoordinatorService/AuthMintCoordinatorRelocation",
  "/roost.v1.CoordinatorService/AuthRedeemCoordinatorRelocation",
  "/roost.v1.CoordinatorService/CoordinatorMovePreflight",
  "/roost.v1.CoordinatorService/CoordinatorMoveStart",
  "/roost.v1.CoordinatorService/CoordinatorMoveStatus",
  "/roost.v1.CoordinatorService/MiscDbExportUrl",
]);

export function isPublicPathDenied(path: string): boolean {
  return PUBLIC_DENIED_PREFIXES.some((prefix) => path.startsWith(prefix))
    || PUBLIC_DENIED_RPCS.has(path);
}

export interface PublicServer extends SyncUpgradeServer {}

export interface PublicSurfaceDeps {
  access: CfAccessVerifier;
  coord: Pick<CoordHandle, "fetch">;
  syncUpgrade?: typeof handleSyncWsUpgrade;
  syncDeps: ConnectDeps;
  move: CoordinatorMoveService;
  cfg: CoordConfig;
  spa: (url: URL, method: string, acceptEncoding: string) => Promise<Response> | Response;
  now?: () => number;
  customLimit?: (clientIp: string, group: string, tokensPerWindow: number) => Response | null;
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
  const security = securityOptionsForConfig(deps.cfg, true);

  async function fetch(req: Request, server: PublicServer): Promise<Response | undefined> {
    const origin = resolveCallerOrigin(
      "public-edge",
      server.requestIP(req)?.address,
      req.headers,
    );
    let accessIdentity;
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
        headers: { "X-Roost-Auth-Layer": "access" },
      }), security, req);
    }

    const url = new URL(req.url);
    if (isPublicPathDenied(url.pathname)) {
      return finalizePublicResponse(new Response("not found", { status: 404 }), security, req);
    }

    const unavailable = coordinatorAvailabilityResponse(
      deps.move.gate.mode,
      req.method,
      url.pathname,
    );
    if (unavailable) return finalizePublicResponse(unavailable, security, req);

    if (url.pathname === "/ws/coord-sync") {
      const limited = customLimit(origin.clientIp, "public-sync-upgrade", 100);
      if (limited) return finalizePublicResponse(limited, security, req);
      const reauthAtMs = Math.min(accessIdentity.exp * 1000, now() + 300_000);
      const upgraded = await syncUpgrade(req, server, deps.syncDeps, reauthAtMs);
      if (upgraded === undefined) return undefined;
      return finalizePublicResponse(
        upgraded ?? new Response("not found", { status: 404 }),
        security,
        req,
      );
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
