// Connect-RPC client singleton for the coordinator. Wire format is protobuf
// binary (useBinaryFormat — no JSON bloat); auth rides the Authorization
// Bearer JWT set per-call by the interceptor, with x-roost-tab-id so coord
// can correlate the three SPA tabs sharing one device key.
//
// Header names and the x-roost-auth-layer sentinels are a cross-app contract
// (coord's middleware classifies on them) — import them from
// @roost/shared/wire/headers, never re-type the literals.

import { Code, ConnectError, createClient, type Interceptor } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";
import { CoordinatorService, type WorkersListResponse } from "@roost/shared/proto/coordinator_pb";
import { signCoordinatorJwt } from "./auth/web-key.ts";
import { getTabId } from "./auth/tab-id.ts";
import { peekCapturedFragmentCredential } from "./auth/fragment-credential.ts";
import {
  storedTenantRouteKey,
  tenantCoordinatorBaseUrl,
} from "./auth/tenant-routing.ts";
import { signal } from "@roost/shared/diag";
import { selectedDashboardId } from "./store/root.ts";
import {
  AUTH_LAYER_ACCESS,
  AUTH_LAYER_DEVICE,
  X_ROOST_AUTH_LAYER,
  X_ROOST_DASHBOARD_ID,
  X_ROOST_TAB_ID,
} from "@roost/shared/wire/headers";
const COORDINATOR_OVERRIDE_KEY = "roost.coordinatorUrl";
let fixedCoordinatorClientRouteGeneration = 0;

/** Permanently retire this document's route-bound singleton. Explicit clients
 * created for a newly resolved route remain usable until navigation reloads
 * the singleton against that route. */
export function invalidateFixedCoordinatorClientForTenantRouteSwitch(): void {
  fixedCoordinatorClientRouteGeneration++;
}
const DEPLOYMENT_MODE_KEY = "roost.deploymentMode";
const DEVICE_AUTH_REQUIRED_PATHS: Record<string, true | undefined> = {
  "/roost.v1.CoordinatorService/AuthDashboardAccess": true,
  "/roost.v1.CoordinatorService/WorkersList": true,
  "/roost.v1.CoordinatorService/SessionsList": true,
  "/roost.v1.CoordinatorService/WorkspacesList": true,
  "/roost.v1.CoordinatorService/TasksList": true,
  "/roost.v1.CoordinatorService/McpList": true,
  "/roost.v1.CoordinatorService/DevicesList": true,
  "/roost.v1.CoordinatorService/DevicesRevoke": true,
  "/roost.v1.CoordinatorService/DevicesRotateCurrent": true,
};

export type AuthFailureKind = "access" | "device" | "retryable";

export class AccessLayerAuthError extends Error {
  constructor() {
    super("Cloudflare Access authentication required");
    this.name = "AccessLayerAuthError";
  }
}

export function classifyAuthFailure(error: unknown, rpcPath: string): AuthFailureKind {
  let current: unknown = error;
  for (let depth = 0; depth < 4 && current; depth++) {
    if (current instanceof AccessLayerAuthError) return "access";
    if (
      current instanceof ConnectError
      && current.code === Code.Unauthenticated
      && current.metadata.get(X_ROOST_AUTH_LAYER) === AUTH_LAYER_DEVICE
      && DEVICE_AUTH_REQUIRED_PATHS[rpcPath]
    ) return "device";
    if (typeof current === "object" && "cause" in current) {
      current = current.cause;
    } else {
      current = undefined;
    }
  }
  return "retryable";
}

const accessAwareFetch: typeof fetch = Object.assign(
  async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
    const response = await globalThis.fetch(input, init);
    if (
      response.status === 401
      && response.headers.get(X_ROOST_AUTH_LAYER) === AUTH_LAYER_ACCESS
    ) {
      throw new AccessLayerAuthError();
    }
    return response;
  },
  { preconnect: globalThis.fetch.preconnect },
);


function capturedTenantRouteKey(): string | null {
  const captured = peekCapturedFragmentCredential();
  return captured?.kind === "activation"
    || captured?.kind === "reset"
    || captured?.kind === "pair"
    ? captured.routeKey ?? null
    : null;
}

function selectedTenantRouteKey(): string | null {
  return storedTenantRouteKey() ?? capturedTenantRouteKey();
}

// Coord URL: localStorage override for multi-coord testing (R6.2);
// defaults to same-origin (proxied by Vite to :4102 in dev, served
// same-origin in prod by coord).

/** The coordinator this SPA is actually talking to: the managed account route
 * when selected, otherwise the Settings → Connection self-hosted override,
 * otherwise same-origin. Worker and WebSocket callers must use this too. */
export function coordBase(): string {
  // A relocation URL is deliberately same-origin on its destination. Ignore a
  // stale per-browser override before creating the singleton transport, or the
  // one-time destination redemption would be sent back to the retired source.
  if (peekCapturedFragmentCredential()?.kind === "relocation") {
    try { localStorage.removeItem(COORDINATOR_OVERRIDE_KEY); } catch { /* storage unavailable */ }
    return "";
  }
  const tenantRouteKey = selectedTenantRouteKey();
  if (tenantRouteKey) return tenantCoordinatorBaseUrl(tenantRouteKey);
  if (typeof localStorage === "undefined") return "";
  const confirmedMode = localStorage.getItem(DEPLOYMENT_MODE_KEY);
  if (confirmedMode !== "self-hosted") return "";
  return localStorage.getItem(COORDINATOR_OVERRIDE_KEY) ?? "";
}

/** Persist the same-origin discovery result before any protected client use.
 * Returning true means the already-created protected transport may target the
 * wrong origin and the caller must reload once. */
export function reconcileCoordinatorOverrideAfterDiscovery(saasMode: boolean): boolean {
  if (typeof localStorage === "undefined") return false;
  const override = localStorage.getItem(COORDINATOR_OVERRIDE_KEY);
  const previousMode = localStorage.getItem(DEPLOYMENT_MODE_KEY);
  if (saasMode) {
    localStorage.removeItem(COORDINATOR_OVERRIDE_KEY);
    localStorage.removeItem(DEPLOYMENT_MODE_KEY);
    return override !== null || previousMode === "self-hosted";
  }
  localStorage.setItem(DEPLOYMENT_MODE_KEY, "self-hosted");
  return override !== null && previousMode !== "self-hosted";
}

function sameOriginBase(): string {
  return typeof location === "undefined" ? "http://localhost" : location.origin;
}
export function coordinatorBaseUrl(): string {
  return coordBase() || sameOriginBase();
}

export function coordinatorRpcUrl(path: `/${string}`): string {
  return `${coordinatorBaseUrl()}${path}`;
}


function makeAuthInterceptor(
  signer: () => Promise<string>,
  expectedRouteGeneration: number | null,
): Interceptor {
  return (next) => async (req) => {
    if (
      expectedRouteGeneration !== null
      && expectedRouteGeneration !== fixedCoordinatorClientRouteGeneration
    ) throw new TypeError("coordinator route changed; reload required");
    try {
      const jwt = await signer();
      req.header.set("Authorization", `Bearer ${jwt}`);
    } catch (error) {
      signal("auth.jwt_sign_fail", {
        stage: "interceptor",
        msg: String(error),
        cooldownKey: "jwt",
      });
    }
    req.header.set(X_ROOST_TAB_ID, getTabId());
    // A caller may supply a server-confirmed switch candidate specifically to
    // AuthDashboardAccess. Every ordinary RPC receives only the current
    // confirmed selection.
    const dashboard = selectedDashboardId();
    if (dashboard && !req.header.has(X_ROOST_DASHBOARD_ID)) {
      req.header.set(X_ROOST_DASHBOARD_ID, dashboard);
    }
    const response = await next(req);
    if (
      expectedRouteGeneration !== null
      && expectedRouteGeneration !== fixedCoordinatorClientRouteGeneration
    ) throw new TypeError("coordinator route changed; reload required");
    return response;
  };
}

function createCoordinatorClientForSigner(
  signer: () => Promise<string>,
  baseUrl: string,
  routeBound: boolean,
) {
  const transport = createConnectTransport({
    baseUrl,
    useBinaryFormat: true,
    fetch: accessAwareFetch,
    interceptors: [makeAuthInterceptor(
      signer,
      routeBound ? fixedCoordinatorClientRouteGeneration : null,
    )],
  });
  return createClient(CoordinatorService, transport);
}

export function makeCoordinatorClientForSigner(
  signer: () => Promise<string>,
  baseUrl = coordinatorBaseUrl(),
) {
  return createCoordinatorClientForSigner(signer, baseUrl, false);
}

/** Public pre-device client for managed login, activation, and recovery. It
 * deliberately sends neither a device JWT nor a dashboard hint. An explicit
 * route key makes email-selected requests independent of module singletons. */
export function makePublicCoordinatorClient(routeKey?: string) {
  const selectedRouteKey = routeKey ?? selectedTenantRouteKey();
  const transport = createConnectTransport({
    baseUrl: selectedRouteKey
      ? tenantCoordinatorBaseUrl(selectedRouteKey)
      : sameOriginBase(),
    useBinaryFormat: true,
    fetch: accessAwareFetch,
    interceptors: [
      (next) => async (req) => {
        req.header.set(X_ROOST_TAB_ID, getTabId());
        return next(req);
      },
    ],
  });
  return createClient(CoordinatorService, transport);
}

export const publicCoordClient = makePublicCoordinatorClient();

export const coordClient = createCoordinatorClientForSigner(
  signCoordinatorJwt,
  coordinatorBaseUrl(),
  true,
);

type Assert<T extends true> = T;
type AsyncResponse<T> = T extends (...args: never[]) => Promise<infer Response> ? Response : never;
type _WorkersListResponseIsTyped = Assert<
  AsyncResponse<typeof coordClient.workersList> extends WorkersListResponse ? true : false
>;
