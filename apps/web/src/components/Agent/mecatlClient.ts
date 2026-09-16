// Builds one Mecatl SDK client per machine for the /agent pane, and classifies
// the coordinator relay's failure bodies into the states the pane renders.
// Every call rides the coordinator BFF at /api/mecatl/<workerFp>/v1/… carrying
// the same device JWT signCoordinatorJwt mints for Connect RPCs (connect.ts),
// so browser JavaScript never holds a Mecatl daemon credential — the daemon
// bearer is added on the worker and stops there.

import { connect, MecatlError, ServerFeature, type Client } from "@stacklok-oss/mecatl-sdk";
import { signal } from "@roost/shared/diag";
import { MecatlUnavailableReasonSchema } from "@roost/shared/mecatl-runtime";
import { signCoordinatorJwt } from "../../auth/web-key.ts";
import { coordinatorRpcUrl } from "../../connect.ts";

/** Why the pane cannot talk to a machine's Mecatl daemon right now. */
export type AgentRelayFailure =
  | { kind: "unavailable"; reason: string }
  | { kind: "offline" }
  | { kind: "busy"; reason: string }
  | { kind: "unauthorized" }
  | { kind: "unknown_machine" }
  | { kind: "failed"; message: string };

// Refusals that clear on their own: retrying is the correct response, so these
// must never read as "Mecatl is not installed". `not_ready` is the ordinary
// cold-boot readiness window and `stopped` is a worker shutting its daemon
// down, so both are waits rather than a machine an operator must go fix.
const TRANSIENT_RELAY_REASONS: Record<string, true | undefined> = {
  not_ready: true,
  stopped: true,
  relay_busy: true,
  upstream_idle: true,
  transport_closed: true,
  response_too_large: true,
};

// Worker daemon-state reasons that need operator action. The coordinator
// returns them verbatim as 502 {"error":"<reason>"}; the SDK surfaces that
// body as the MecatlError message because its problem decoder falls back to
// `error`. Derived from the shared enum rather than retyped, so a reason added
// on the worker cannot fall through here as a generic failure.
const DAEMON_UNAVAILABLE_REASONS: Record<string, true | undefined> = Object.fromEntries(
  MecatlUnavailableReasonSchema.options
    .filter((reason) => !TRANSIENT_RELAY_REASONS[reason])
    .map((reason) => [reason, true]),
);

/**
 * Creates the SDK client for one machine. The caller owns `close()`.
 *
 * The relative-vs-absolute base matters: a browser pointed at another
 * coordinator through Settings → Connection must relay through THAT origin,
 * which is what coordinatorRpcUrl resolves.
 */
export function createMecatlClient(workerFp: string): Client {
  return connect({
    baseUrl: coordinatorRpcUrl(`/api/mecatl/${workerFp}`),
    // The repo typechecks with `types: ["bun"]`, which widens
    // globalThis.fetch with Bun's `preconnect` hint that no browser
    // implements and the SDK never calls. The wrapper carries the request
    // signature the transport actually uses.
    fetch: signedRelayFetch as typeof globalThis.fetch,
  });
}

/** True when this deployment's Mecatl advertises mid-run steering over HTTP. */
export function serverSupportsHttpSteer(features: ReadonlySet<string>): boolean {
  return features.has(ServerFeature.HttpSteer);
}

/**
 * Maps a thrown SDK error onto the pane's failure vocabulary. Status is read
 * before the body so a proxy that replaced the JSON problem still classifies.
 */
export function classifyRelayFailure(error: unknown): AgentRelayFailure {
  if (!(error instanceof MecatlError)) {
    return { kind: "failed", message: describeThrown(error) };
  }
  const reason = error.message.trim();
  switch (error.status) {
    case 401:
    case 403:
      return { kind: "unauthorized" };
    case 404:
      return { kind: "unknown_machine" };
    case 429:
    case 504:
      return { kind: "busy", reason: reason || "relay_busy" };
    case 503:
      return { kind: "offline" };
    default:
      break;
  }
  if (DAEMON_UNAVAILABLE_REASONS[reason]) return { kind: "unavailable", reason };
  if (TRANSIENT_RELAY_REASONS[reason]) return { kind: "busy", reason };
  // A 502 with an unrecognized reason still means the machine answered and
  // could not serve Mecatl, which is the unavailable state, not a pane bug.
  if (error.status === 502) return { kind: "unavailable", reason: reason || "daemon_exit" };
  return { kind: "failed", message: reason || describeThrown(error) };
}

/** True for the abort the pane itself raised on unmount or session change. */
export function isAbortFailure(error: unknown): boolean {
  if (error instanceof DOMException) return error.name === "AbortError";
  return error instanceof Error && error.name === "AbortError";
}

function describeThrown(error: unknown): string {
  if (error instanceof Error) return error.message || error.name;
  return String(error);
}

// The SDK binds this to globalThis and calls it for every unary request and
// SSE stream, so the JWT is minted per call and rides the cached-token fast
// path in web-key.ts rather than being captured once at client construction.
async function signedRelayFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const headers = new Headers(init?.headers);
  try {
    headers.set("Authorization", `Bearer ${await signCoordinatorJwt()}`);
  } catch (error) {
    signal("auth.jwt_sign_fail", {
      stage: "mecatl_relay",
      msg: String(error),
      cooldownKey: "jwt",
    });
  }
  return await fetch(input, { ...init, headers });
}
