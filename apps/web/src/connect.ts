// Connect-RPC client init. crpc1 PoC ships CoordinatorService.WorkersList
// alongside the existing tRPC client. crpc2 will migrate all call sites.
// crpc6 will delete trpc.ts entirely.
//
// Wire format: protobuf binary (useBinaryFormat: true) — no JSON bloat.
// Auth: Authorization: Bearer <jwt> via Interceptor, same JWT as tRPC.

import { createClient, type Interceptor } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";
import { CoordinatorService, type WorkersListResponse } from "@roost/shared/proto/coordinator_pb";
import { signCoordinatorJwt } from "./auth/web-key.ts";
import { getTabId } from "./auth/tab-id.ts";
import { signal } from "@roost/shared/diag";

// Coord URL: localStorage override for multi-coord testing (R6.2);
// defaults to same-origin (proxied by Vite to :4102 in dev, served
// same-origin in prod by coord).
export function hasCoordinatorRelocationFragment(): boolean {
  if (typeof location === "undefined") return false;
  const params = new URLSearchParams(location.hash.slice(1));
  return Boolean(params.get("move") && params.get("handoff"));
}

function coordBase(): string {
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

// Stamp Authorization header on every Connect call.
const authInterceptor: Interceptor = (next) => async (req) => {
  try {
    const jwt = await signCoordinatorJwt();
    req.header.set("Authorization", `Bearer ${jwt}`);
  } catch (e) {
    // signCoordinatorJwt can fail on first-paint before keypair generated;
    // procedure rejects with UNAUTHORIZED, SPA surfaces the auth banner.
    // But a persistent sign failure = every RPC goes out unauthenticated.
    signal("auth.jwt_sign_fail", { stage: "interceptor", msg: String(e), cooldownKey: "jwt" });
  }
  // Per-tab UUID so coord/worker can disambiguate multiple tabs from
  // the same browser fingerprint in the viewport-claim maps. Same
  // browser, two tabs = same JWT subject (fp), distinct tab_id.
  req.header.set("x-roost-tab-id", getTabId());
  return next(req);
};

const transport = createConnectTransport({
  baseUrl: coordBase() || "/",
  useBinaryFormat: true,  // protobuf binary on the wire; ~80% smaller than JSON
  interceptors: [authInterceptor],
});

export const coordClient = createClient(CoordinatorService, transport);

type Assert<T extends true> = T;
type AsyncResponse<T> = T extends (...args: never[]) => Promise<infer Response> ? Response : never;
type _WorkersListResponseIsTyped = Assert<
  AsyncResponse<typeof coordClient.workersList> extends WorkersListResponse ? true : false
>;
