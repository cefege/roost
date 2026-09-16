// Coordinator BFF for the per-machine Mecatl daemon. A browser calls
// /api/mecatl/<workerFp>/v1/... on this origin; this module authenticates the
// device, then relays the exchange over the existing worker link as opaque
// HTTP. Called by coord-factory.ts; chunks arrive from worker-frame-dispatch.ts.
// Roost models no agent semantics here and persists nothing from the exchange.

import { verifyJwt, type JwtCache } from "./jwt.ts";
import { resolveCallerPrincipal, type AccountDeviceCaller } from "./connect/auth-principal.ts";
import { sendMecatlRelayCancel, sendMecatlRelayRequest } from "./connect/worker-send.ts";
import { log } from "@roost/shared/log";
import type { CoordConfig } from "@roost/shared/config";
import type { KyselyDB } from "./db/connection.ts";
import type { WMecatlRelayChunk } from "@roost/shared/proto/worker_transport_pb";

const RELAY_PATH_PREFIX = "/api/mecatl/";
/** Every relayed path is a Mecatl v1 API call; nothing else is proxied. */
const RELAY_PATH_ROOT = "/v1/";
/** Hex SHA-256 of an ed25519 public key: bound the segment before SQLite. */
const WORKER_FP_MAX_CHARS = 64;
const RELAY_MAX_PER_WORKER = 16;
/** Unread browser-bound bytes one relay may hold before it is abandoned. */
const RELAY_UNREAD_BYTES_MAX = 512 * 1024;
/** Silence from the daemon that ends the exchange. SSE keepalives make this
 * safe; the worker's own idle bound is shorter, so this only covers a link
 * that stopped delivering without reporting an error. */
const RELAY_IDLE_TIMEOUT_MS = 90_000;
/** Only these response headers survive the hop. Length and framing headers
 * (`content-length`, `transfer-encoding`, `connection`) describe the daemon's
 * socket rather than this one, and a cancelled relay makes them lies. */
const FORWARDED_RESPONSE_HEADERS: Record<string, true> = {
  "content-type": true,
  "cache-control": true,
};
const DEFAULT_RESPONSE_CONTENT_TYPE = "application/octet-stream";
/** A header value long enough to be an attack is not a real media type. */
const HEADER_VALUE_MAX_CHARS = 512;

export interface MecatlRelayDeps {
  db: KyselyDB;
  cfg: CoordConfig;
  jwtCache: JwtCache;
}

interface RelayEntry {
  workerFp: string;
  requestId: string;
  /** Resolves once: the streaming response built from the `head` chunk, or the
   * error response that arrived instead of one. */
  response: Promise<Response>;
  settle: (response: Response) => void;
  controller: ReadableStreamDefaultController<Uint8Array> | null;
  headSeen: boolean;
  finished: boolean;
  idleTimer: Timer | undefined;
}

/** In-flight relays keyed `<workerFp>\0<requestId>`, so one worker can never
 * settle another's exchange, plus the per-worker slot count the cap reads. */
const pendingRelays = new Map<string, RelayEntry>();
const relayCounts = new Map<string, number>();

export async function handleMecatlRelay(
  req: Request,
  url: URL,
  deps: MecatlRelayDeps,
): Promise<Response> {
  const target = parseRelayTarget(url);
  if (!target) return relayError(404, "bad_path");

  const principal = await resolveRelayPrincipal(req, deps);
  if (!principal) return relayError(401, "unauthorized");

  const liveWorker = await deps.db
    .selectFrom("workers")
    .select("fp")
    .where("fp", "=", target.workerFp)
    .where("deleted_at_ms", "is", null)
    .executeTakeFirst();
  if (!liveWorker) return relayError(404, "unknown_worker");

  if ((relayCounts.get(target.workerFp) ?? 0) >= RELAY_MAX_PER_WORKER) {
    return relayError(429, "relay_busy");
  }

  const body = new Uint8Array(await req.arrayBuffer());
  const requestId = crypto.randomUUID();
  const entry = registerRelay(target.workerFp, requestId);
  const admitted = sendMecatlRelayRequest(target.workerFp, {
    requestId,
    method: req.method,
    path: target.path,
    headersJson: JSON.stringify(forwardedRequestHeaders(req.headers)),
    body,
  });
  if (!admitted) {
    // Nothing reached the worker, so there is nothing to cancel; closing the
    // registration is what produces the refusal this caller receives.
    closeRelay(entry, "worker_offline", 503);
    return entry.response;
  }
  log.info("mecatl-relay", "relay_started", {
    worker_fp: target.workerFp,
    request_id: requestId,
    method: req.method,
    request_bytes: body.length,
  });
  // The browser giving up must reach the daemon: an abandoned run would keep
  // burning model tokens on that machine.
  req.signal.addEventListener("abort", () => cancelRelay(entry, "caller_aborted", 499), {
    once: true,
  });
  // An abort that fired while this handler awaited the database never
  // dispatches to a listener added now, so it would otherwise linger.
  if (req.signal.aborted) cancelRelay(entry, "caller_aborted", 499);
  return entry.response;
}

/** One upstream `WMecatlRelayChunk` from the authenticated worker. An unknown
 * request id is the normal race against a relay this side already dropped. */
export function deliverMecatlRelayChunk(workerFp: string, chunk: WMecatlRelayChunk): void {
  const entry = pendingRelays.get(relayKey(workerFp, chunk.requestId));
  if (!entry || entry.finished) return;
  armIdleTimer(entry);

  if (chunk.head && !entry.headSeen) openRelayStream(entry, chunk);

  const controller = entry.controller;
  if (chunk.body.length > 0 && controller) {
    try {
      controller.enqueue(chunk.body);
    } catch {
      // The browser stream is gone; the daemon must stop producing.
      cancelRelay(entry, "caller_gone", 499);
      return;
    }
    if ((controller.desiredSize ?? 0) <= 0) {
      // A stalled reader would otherwise pin unbounded coordinator memory.
      cancelRelay(entry, "unread_ceiling", 502);
      return;
    }
  }

  if (!chunk.end) return;
  // A refusal that arrives before any head is the browser's whole answer;
  // after the head the response already shipped, so the stream just ends.
  closeRelay(entry, chunk.error || "transport_closed", 502);
}

function relayKey(workerFp: string, requestId: string): string {
  return `${workerFp}\u0000${requestId}`;
}

/** `/api/mecatl/<workerFp>/v1/...` → the fingerprint plus the daemon-relative
 * path. The route branch in coord-factory.ts holds the same prefix, so this
 * re-checks it: a divergence must refuse, never mis-parse a fingerprint. */
function parseRelayTarget(url: URL): { workerFp: string; path: string } | null {
  if (!url.pathname.startsWith(RELAY_PATH_PREFIX)) return null;
  const rest = url.pathname.slice(RELAY_PATH_PREFIX.length);
  const boundary = rest.indexOf("/");
  if (boundary <= 0 || boundary > WORKER_FP_MAX_CHARS) return null;
  const path = rest.slice(boundary) + url.search;
  if (!path.startsWith(RELAY_PATH_ROOT)) return null;
  return { workerFp: rest.slice(0, boundary), path };
}

/** The whole authorization model: an authenticated device reaches the install.
 * A worker principal carries no browser authority and is refused. */
async function resolveRelayPrincipal(
  req: Request,
  deps: MecatlRelayDeps,
): Promise<AccountDeviceCaller | null> {
  const authorization = req.headers.get("authorization");
  if (!authorization?.startsWith("Bearer ")) return null;
  const token = authorization.slice(7);
  if (!token) return null;
  try {
    const verified = await verifyJwt(token, {
      db: deps.db,
      cache: deps.jwtCache,
      jwtMaxAgeSecs: deps.cfg.jwtMaxAgeSecs,
    });
    const principal = await resolveCallerPrincipal(deps.db, verified);
    if (principal?.kind === "account-device" || principal?.kind === "legacy-self-hosted") {
      return principal;
    }
  } catch (error) {
    log.warn("mecatl-relay", "auth_rejected", { error: String(error) });
  }
  return null;
}

/** The Roost JWT and every other browser header stop here. */
function forwardedRequestHeaders(headers: Headers): Record<string, string> {
  const forwarded: Record<string, string> = {};
  const contentType = headers.get("content-type");
  if (contentType) forwarded["content-type"] = contentType;
  const accept = headers.get("accept");
  if (accept) forwarded.accept = accept;
  return forwarded;
}

function registerRelay(workerFp: string, requestId: string): RelayEntry {
  let settle!: (response: Response) => void;
  const response = new Promise<Response>((resolve) => {
    settle = resolve;
  });
  const entry: RelayEntry = {
    workerFp,
    requestId,
    response,
    settle,
    controller: null,
    headSeen: false,
    finished: false,
    idleTimer: undefined,
  };
  pendingRelays.set(relayKey(workerFp, requestId), entry);
  relayCounts.set(workerFp, (relayCounts.get(workerFp) ?? 0) + 1);
  armIdleTimer(entry);
  return entry;
}

function armIdleTimer(entry: RelayEntry): void {
  clearTimeout(entry.idleTimer);
  entry.idleTimer = setTimeout(
    () => cancelRelay(entry, "upstream_idle", 504),
    RELAY_IDLE_TIMEOUT_MS,
  );
}

function openRelayStream(entry: RelayEntry, head: WMecatlRelayChunk): void {
  entry.headSeen = true;
  const stream = new ReadableStream<Uint8Array>({
    start(controller: ReadableStreamDefaultController<Uint8Array>) {
      entry.controller = controller;
    },
    cancel() {
      cancelRelay(entry, "caller_gone", 499);
    },
  }, new ByteLengthQueuingStrategy({ highWaterMark: RELAY_UNREAD_BYTES_MAX }));
  entry.settle(new Response(stream, {
    status: head.status >= 200 && head.status <= 599 ? head.status : 502,
    headers: forwardedResponseHeaders(head.headersJson),
  }));
}

function forwardedResponseHeaders(headersJson: string): Record<string, string> {
  const forwarded: Record<string, string> = {
    "content-type": DEFAULT_RESPONSE_CONTENT_TYPE,
  };
  let parsed: unknown;
  try {
    parsed = JSON.parse(headersJson);
  } catch {
    // A malformed header map cannot describe the body it came with.
    return forwarded;
  }
  if (typeof parsed !== "object" || parsed === null) return forwarded;
  for (const [name, value] of Object.entries(parsed as Record<string, unknown>)) {
    const lowered = name.toLowerCase();
    if (typeof value !== "string" || value.length === 0) continue;
    if (value.length > HEADER_VALUE_MAX_CHARS) continue;
    if (!FORWARDED_RESPONSE_HEADERS[lowered]) continue;
    forwarded[lowered] = value;
  }
  return forwarded;
}

/** Stop the daemon work, then settle this side. An abandoned exchange fails
 * the browser stream: a clean close would be indistinguishable from a complete
 * body, so a truncated response would read as a valid short one. */
function cancelRelay(entry: RelayEntry, reason: string, unsentStatus: number): void {
  if (entry.finished) return;
  sendMecatlRelayCancel(entry.workerFp, entry.requestId);
  const controller = entry.controller;
  if (controller) {
    entry.controller = null;
    controller.error(new Error(`mecatl relay ${reason}`));
  }
  closeRelay(entry, reason, unsentStatus);
}

/** Terminal for one relay: release the slot, then either close the browser
 * stream or answer a request that never received its `head`. */
function closeRelay(entry: RelayEntry, reason: string, unsentStatus: number): void {
  if (entry.finished) return;
  entry.finished = true;
  clearTimeout(entry.idleTimer);
  entry.idleTimer = undefined;
  const key = relayKey(entry.workerFp, entry.requestId);
  if (pendingRelays.get(key) === entry) {
    pendingRelays.delete(key);
    const remaining = (relayCounts.get(entry.workerFp) ?? 1) - 1;
    if (remaining > 0) relayCounts.set(entry.workerFp, remaining);
    else relayCounts.delete(entry.workerFp);
  }
  if (entry.controller) {
    try {
      entry.controller.close();
    } catch {
      // Already closed or cancelled by the consumer.
    }
    entry.controller = null;
  } else {
    entry.settle(relayError(unsentStatus, reason));
  }
  log.info("mecatl-relay", "relay_finished", {
    worker_fp: entry.workerFp,
    request_id: entry.requestId,
    reason,
    head_sent: entry.headSeen,
  });
}

function relayError(status: number, reason: string): Response {
  return new Response(JSON.stringify({ error: reason }), {
    status,
    headers: { "content-type": "application/json" },
  });
}
