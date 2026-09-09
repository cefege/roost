// Connect-RPC client singleton for the coordinator. Wire format is protobuf
// binary (useBinaryFormat — no JSON bloat); auth rides the Authorization
// Bearer JWT set per-call by the interceptor, with x-roost-tab-id so coord
// can correlate the three SPA tabs sharing one device key.
//
// Header names and the x-roost-auth-layer sentinel are a cross-app contract
// (coord's middleware classifies on them) — import them from
// @roost/shared/wire/headers, never re-type the literals.

import { Code, ConnectError, createClient, type Interceptor } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";
import { CoordinatorService, type WorkersListResponse } from "@roost/shared/proto/coordinator_pb";
import { signCoordinatorJwt } from "./auth/web-key.ts";
import { getTabId } from "./auth/tab-id.ts";
import { signal } from "@roost/shared/diag";
import {
  AUTH_LAYER_DEVICE,
  X_ROOST_AUTH_LAYER,
  X_ROOST_TAB_ID,
} from "@roost/shared/wire/headers";
const COORDINATOR_OVERRIDE_KEY = "roost.coordinatorUrl";
const DEPLOYMENT_MODE_KEY = "roost.deploymentMode";
const DEVICE_AUTH_REQUIRED_PATHS: Record<string, true | undefined> = {
  "/roost.v1.CoordinatorService/WorkersList": true,
  "/roost.v1.CoordinatorService/SessionsList": true,
  "/roost.v1.CoordinatorService/WorkspacesList": true,
  "/roost.v1.CoordinatorService/TasksList": true,
  "/roost.v1.CoordinatorService/McpList": true,
  "/roost.v1.CoordinatorService/DevicesList": true,
  "/roost.v1.CoordinatorService/DevicesRevoke": true,
  "/roost.v1.CoordinatorService/DevicesRotateCurrent": true,
};

export type AuthFailureKind = "device" | "retryable";

export function classifyAuthFailure(error: unknown, rpcPath: string): AuthFailureKind {
  let current: unknown = error;
  for (let depth = 0; depth < 4 && current; depth++) {
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


// Coord URL: the Settings → Connection override for pointing this browser at a
// coordinator on another origin; otherwise same-origin (proxied by Vite to
// :4102 in dev, served same-origin in prod by coord).

/** The coordinator this SPA is actually talking to: the Settings → Connection
 * override once same-origin discovery has confirmed this deployment, otherwise
 * same-origin. Worker and WebSocket callers must use this too. */
export function coordBase(): string {
  if (typeof localStorage === "undefined") return "";
  if (localStorage.getItem(DEPLOYMENT_MODE_KEY) !== "self-hosted") return "";
  return localStorage.getItem(COORDINATOR_OVERRIDE_KEY) ?? "";
}

/** Persist the same-origin discovery result before any protected client use.
 * Returning true means the already-created protected transport may target the
 * wrong origin and the caller must reload once. */
export function reconcileCoordinatorOverrideAfterDiscovery(): boolean {
  if (typeof localStorage === "undefined") return false;
  const override = localStorage.getItem(COORDINATOR_OVERRIDE_KEY);
  const previousMode = localStorage.getItem(DEPLOYMENT_MODE_KEY);
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


function makeAuthInterceptor(signer: () => Promise<string>): Interceptor {
  return (next) => async (req) => {
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
    return next(req);
  };
}

export function makeCoordinatorClientForSigner(
  signer: () => Promise<string>,
  baseUrl = coordinatorBaseUrl(),
) {
  return createClient(CoordinatorService, createConnectTransport({
    baseUrl,
    useBinaryFormat: true,
    interceptors: [makeAuthInterceptor(signer)],
  }));
}

/** Public pre-device client used by identity discovery. It deliberately sends
 * no device JWT, and always targets same-origin so discovery cannot be steered
 * by a stale coordinator override. */
export const publicCoordClient = createClient(
  CoordinatorService,
  createConnectTransport({
    baseUrl: sameOriginBase(),
    useBinaryFormat: true,
    interceptors: [
      (next) => async (req) => {
        req.header.set(X_ROOST_TAB_ID, getTabId());
        return next(req);
      },
    ],
  }),
);

export const coordClient = makeCoordinatorClientForSigner(
  signCoordinatorJwt,
  coordinatorBaseUrl(),
);

type Assert<T extends true> = T;
type AsyncResponse<T> = T extends (...args: never[]) => Promise<infer Response> ? Response : never;
type _WorkersListResponseIsTyped = Assert<
  AsyncResponse<typeof coordClient.workersList> extends WorkersListResponse ? true : false
>;
