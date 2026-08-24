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
import { parseFragmentCredential } from "./auth/fragment-credential.ts";
import { signal } from "@roost/shared/diag";
import {
  AUTH_LAYER_ACCESS,
  AUTH_LAYER_DEVICE,
  X_ROOST_AUTH_LAYER,
  X_ROOST_TAB_ID,
} from "@roost/shared/wire/headers";
const DEVICE_AUTH_REQUIRED_PATHS: Record<string, true | undefined> = {
  "/roost.v1.CoordinatorService/WorkersList": true,
  "/roost.v1.CoordinatorService/SessionsList": true,
  "/roost.v1.CoordinatorService/WorkspacesList": true,
  "/roost.v1.CoordinatorService/TasksList": true,
  "/roost.v1.CoordinatorService/PermissionsList": true,
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


// Coord URL: localStorage override for multi-coord testing (R6.2);
// defaults to same-origin (proxied by Vite to :4102 in dev, served
// same-origin in prod by coord).
export function hasCoordinatorRelocationFragment(): boolean {
  if (typeof location === "undefined") return false;
  return parseFragmentCredential(location.hash).kind === "relocation";
}

/** The coordinator this SPA is actually talking to: the Settings → Connection
 *  override when set, else same-origin. Callers that build install commands
 *  must use this, not `location.origin`. */
export function coordBase(): string {
  // A relocation URL is deliberately same-origin on its destination. Ignore a
  // stale per-browser override before creating the singleton transport, or the
  // one-time destination redemption would be sent back to the retired source.
  if (hasCoordinatorRelocationFragment()) {
    try { localStorage.removeItem("roost.coordinatorUrl"); } catch { /* storage unavailable */ }
    return "";
  }
  const override = typeof localStorage !== "undefined"
    ? localStorage.getItem("roost.coordinatorUrl")
    : null;
  return override ?? "";
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

export function makeCoordinatorClientForSigner(signer: () => Promise<string>) {
  const transport = createConnectTransport({
    baseUrl: coordBase() || "/",
    useBinaryFormat: true,
    fetch: accessAwareFetch,
    interceptors: [makeAuthInterceptor(signer)],
  });
  return createClient(CoordinatorService, transport);
}

export const coordClient = makeCoordinatorClientForSigner(signCoordinatorJwt);

type Assert<T extends true> = T;
type AsyncResponse<T> = T extends (...args: never[]) => Promise<infer Response> ? Response : never;
type _WorkersListResponseIsTyped = Assert<
  AsyncResponse<typeof coordClient.workersList> extends WorkersListResponse ? true : false
>;
