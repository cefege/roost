// Owns Sync WebSocket handshake validation and the data handed to Bun's socket.
// The fetch surface calls it before the live handler takes over the connection.
// Keeping authorization and initialization together preserves rejection order
// without coupling request-only state to the long-lived delivery closure.

import { verifyJwt, type Caller as VerifiedJwtCaller } from "../jwt.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import {
  loadSyncDashboardScope,
  type SyncDashboardScope,
  type SyncFeed,
} from "./sync-feed.ts";
import type { WsAuthDeadlineTimer } from "./ws-auth-deadline.ts";
import {
  createSyncV2SocketState,
  type SyncV2SocketState,
} from "./sync-ws-v2-state.ts";
import type { ConnectDeps } from "./router.ts";
import {
  resolveCallerPrincipal,
  resolveDashboardActor,
} from "./auth-interceptor.ts";
import {
  SYNC_WS_PATH,
  SYNC_AUTH_SUBPROTOCOL,
  SYNC_QUERY_FLOW_V1,
  SYNC_QUERY_V2,
} from "@roost/shared/wire/sync-ws";

export interface SyncUpgradeServer {
  requestIP(req: Request): { address: string } | null;
  upgrade(
    req: Request,
    opts: { data: SyncWsData; headers?: HeadersInit },
  ): boolean;
}

function isAllowedWsOrigin(
  origin: string,
  host: string,
  cfg: ConnectDeps["cfg"],
): boolean {
  if (
    origin === cfg.webPublicUrl
    || origin === cfg.publicUrl
    || cfg.corsAllowedOrigins.includes(origin)
    || origin === `https://${host}`
  ) return true;
  return cfg.relaxedCsp && origin === `http://${host}`;
}

export interface SyncDeliveryRecord {
  readonly seq: bigint;
  readonly encodedBytes: number;
  readonly sentAtMs: number;
}

/** Server-owned dashboard scope shared by account-device and read-only worker
 * Sync callers. Browser-only actor details never become worker claims. */
export interface SyncDashboardActor {
  dashboardId: string;
}

export interface SyncWsData {
  kind: "sync";
  caller: VerifiedJwtCaller;
  /** Resolved from persisted browser membership or workers.dashboard_id before
   * upgrade. The requested query value is never treated as proof. */
  actor: SyncDashboardActor;
  /** Worker Sync is a firehose consumer only; it may ACK/subscription-control
   * delivery but cannot issue terminal view or input commands. */
  readOnly: boolean;
  scope: SyncDashboardScope;
  sinceEventId: number;
  /** `${fingerprint}:${tabId}` identifies the browser tab that owns socket-bound
   * terminal view handles and attributes typed input. A v2 socket without
   * `tab=` remains a read-only firehose consumer and cannot issue either. */
  viewerKey: string | null;
  remoteAddress?: string | null;
  feed: SyncFeed | null;
  keepaliveTimer: Timer | null;
  reauthAtMs: number | null;
  reauthTimer: WsAuthDeadlineTimer | null;
  pressureTimer: Timer | null;
  pressureFrame: string | null;
  pressureClosing: boolean;
  /** Enabled only by the exact `flow=1` upgrade query value. */
  flowControl: boolean;
  /** Present only for the exact `flow=1&sync_v=2` capability negotiation. */
  v2?: SyncV2SocketState;
  /** Last sequence accepted by ws.send (not merely encoded or attempted). */
  lastSentDeliverySeq: bigint;
  /** Highest cumulative ACK accepted from this socket. */
  ackDeliverySeq: bigint;
  unackedEncodedBytes: number;
  /** Metadata only: payloads and encoded buffers must never enter this queue. */
  deliveryQueue: SyncDeliveryRecord[];
  deliveryTimer: Timer | null;
  /** ACK/close notifications for the single retained-seed pacing phase. */
  deliveryWaiters: Set<() => void>;
}

/** Bun fetch-handler hook. Returns null for another path, undefined after a
 * successful hijack, or a rejection response when admission fails. */
export async function handleSyncWsUpgrade(
  req: Request,
  server: SyncUpgradeServer,
  deps: ConnectDeps,
  reauthAtMs: number | null = null,
): Promise<Response | undefined | null> {
  const url = new URL(req.url);
  if (url.pathname !== SYNC_WS_PATH) return null;
  // WS handshakes are GET, so main.ts's retired gate (`req.method !== "GET"`)
  // cannot see them. Any non-active mode must fail fast here, or a browser
  // reconnecting mid-move attaches to a frozen DB and gets keepalives forever
  // instead of falling into the AuthCoordIdentity discovery path.
  if (deps.move && deps.move.gate.mode !== "active") {
    return new Response("coordinator move in progress", {
      status: deps.move.gate.mode === "retired" ? 410 : 503,
    });
  }
  const wsOrigin = req.headers.get("origin");
  if (wsOrigin && !isAllowedWsOrigin(wsOrigin, url.host, deps.cfg)) {
    const addr = server.requestIP(req)?.address ?? undefined;
    signal("sync.auth_rejected", {
      reason: "origin_rejected",
      addr,
      cooldownKey: addr ?? "sync-origin",
    });
    return new Response("forbidden origin", { status: 403 });
  }
  const addr = server.requestIP(req)?.address ?? undefined;
  const protocols = (req.headers.get("sec-websocket-protocol") ?? "")
    .split(",")
    .map((part) => part.trim());
  if (protocols.length !== 2 || protocols[0] !== SYNC_AUTH_SUBPROTOCOL || !protocols[1]) {
    return new Response("unauthorized", { status: 401 });
  }
  const token = protocols[1];
  let caller: VerifiedJwtCaller;
  try {
    caller = await verifyJwt(token, {
      db: deps.db,
      cache: deps.jwtCache,
      jwtMaxAgeSecs: deps.cfg.jwtMaxAgeSecs,
    });
  } catch (error) {
    log.warn("sync-ws", "upgrade_jwt_failed", { error: String(error) });
    signal("sync.auth_rejected", {
      reason: "jwt_invalid",
      addr,
      cooldownKey: addr ?? "sync-auth",
    });
    return new Response("unauthorized", { status: 401 });
  }
  const dashboardId = url.searchParams.get("dashboard");
  if (!dashboardId) return new Response("not found", { status: 404 });
  const principal = await resolveCallerPrincipal(deps.db, deps.cfg, caller);
  let actor: SyncDashboardActor;
  let readOnly: boolean;
  if (principal?.kind === "account-device") {
    const browserActor = await resolveDashboardActor(
      deps.db,
      principal.fingerprint,
      dashboardId,
    );
    if (!browserActor) return new Response("not found", { status: 404 });
    actor = browserActor;
    readOnly = false;
  } else if (principal?.kind === "worker") {
    if (principal.dashboardId !== dashboardId) {
      return new Response("not found", { status: 404 });
    }
    actor = { dashboardId: principal.dashboardId };
    readOnly = true;
  } else {
    // Unmapped legacy keys have no tenant scope. After bootstrap every browser
    // key is an account device and therefore follows the membership path above.
    return new Response("not found", { status: 404 });
  }
  const scope = await loadSyncDashboardScope(deps.db, actor.dashboardId);
  const since = Number(url.searchParams.get("since")) || 0;
  const tabId = url.searchParams.get("tab");
  const flowControl = url.searchParams.get("flow") === SYNC_QUERY_FLOW_V1;
  const syncV2 = flowControl && url.searchParams.get("sync_v") === SYNC_QUERY_V2;
  const credentialDeadlineMs = deps.cfg.saasMode ? caller.validUntilMs : null;
  const socketDeadlineMs = reauthAtMs === null
    ? credentialDeadlineMs
    : credentialDeadlineMs === null
      ? reauthAtMs
      : Math.min(reauthAtMs, credentialDeadlineMs);
  const data: SyncWsData = {
    kind: "sync",
    caller,
    actor,
    scope,
    readOnly,
    sinceEventId: since,
    viewerKey: !readOnly && tabId ? `${caller.fingerprint}:${tabId}` : null,
    remoteAddress: addr ?? null,
    feed: null,
    keepaliveTimer: null,
    reauthAtMs: socketDeadlineMs,
    reauthTimer: null,
    pressureTimer: null,
    pressureFrame: null,
    pressureClosing: false,
    flowControl,
    v2: syncV2 ? createSyncV2SocketState() : undefined,
    lastSentDeliverySeq: 0n,
    ackDeliverySeq: 0n,
    unackedEncodedBytes: 0,
    deliveryQueue: [],
    deliveryTimer: null,
    deliveryWaiters: new Set(),
  };
  const ok = server.upgrade(req, {
    data,
    headers: { "Sec-WebSocket-Protocol": SYNC_AUTH_SUBPROTOCOL },
  });
  if (ok) return undefined;
  return new Response("upgrade failed", { status: 400 });
}
