// Authenticates and upgrades the Bun worker WebSocket before any connection
// state can enter the coordinator. It binds the URL fingerprint to the
// persisted worker principal and builds the announced-channel repair barrier
// that protects terminal frames racing their durable route announcement.

import type { Server } from "bun";
import { jwtKeyGeneration, verifyJwt, type Caller as VerifiedJwtCaller } from "../jwt.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import { WORKER_AUTH_SUBPROTOCOL } from "@roost/shared/wire/coord-worker";
import { resolveCallerPrincipal } from "./auth-interceptor.ts";
import { currentTerminalScreenHub } from "./terminal-view-hub.ts";
import { AnnouncedChannelBarrier } from "./announced-channel-barrier.ts";
import type { WorkerServiceDeps } from "./worker-service.ts";
import type { WorkerWsData } from "./worker-ws-handler.ts";

const WS_PATH_RE = /^\/ws\/coord-worker\/([a-f0-9]{64})$/;

/** Every socket's barrier must report its drops into the coordinator-local
 * repair state, so construction is centralized here. */
export function createAnnouncedChannelBarrier(workerFp: string): AnnouncedChannelBarrier {
  return new AnnouncedChannelBarrier((drop) => {
    currentTerminalScreenHub()?.invalidate(
      drop.sessionId,
      `announced channel barrier ${drop.reason} on ${workerFp.slice(0, 12)}`,
    );
  });
}

/** Bun fetch-handler hook. Returns null for another path, undefined after a
 * successful hijack, or a rejection response when admission fails. */
export async function handleWorkerWsUpgrade(
  req: Request,
  server: Server<WorkerWsData>,
  deps: WorkerServiceDeps,
): Promise<Response | undefined | null> {
  const url = new URL(req.url);
  const match = WS_PATH_RE.exec(url.pathname);
  if (!match) return null;
  // WS handshakes are GET, so main.ts's retired gate cannot see them. Reject
  // ONLY on `retired`: the link must stay open through `source_draining` to
  // buffer unacked events and receive ACTIVATE. Rejecting once retired is what
  // makes coord-relocation-recovery's link-closed guard engage.
  if (deps.move?.gate.mode === "retired") {
    return new Response("coordinator relocated", { status: 410 });
  }
  const fp = match[1]!;
  const addr = server.requestIP?.(req)?.address ?? undefined;
  // This endpoint has no query contract. Rejecting the entire query surface
  // guarantees an old `?token=` client cannot leak a credential into access
  // logs while still authenticating successfully by subprotocol.
  if (url.search !== "") {
    signal("worker.auth_rejected", {
      reason: "query_credential",
      addr,
      cooldownKey: addr ?? "worker-auth",
    });
    return new Response("unauthorized", { status: 401 });
  }
  const protocols = (req.headers.get("sec-websocket-protocol") ?? "")
    .split(",")
    .map((part) => part.trim());
  if (
    protocols.length !== 2
    || protocols[0] !== WORKER_AUTH_SUBPROTOCOL
    || !protocols[1]
  ) {
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
    log.warn("worker-ws", "upgrade_jwt_failed", {
      error: String(error),
      url_fp: fp,
      forwarded_for: req.headers.get("x-forwarded-for"),
    });
    signal("worker.auth_rejected", {
      reason: "jwt_invalid",
      addr,
      cooldownKey: addr ?? "worker-auth",
    });
    return new Response("unauthorized", { status: 401 });
  }
  const principal = await resolveCallerPrincipal(deps.db, deps.cfg, caller);
  if (
    caller.fingerprint !== fp
    || principal?.kind !== "worker"
    || principal.fingerprint !== fp
    || jwtKeyGeneration(deps.jwtCache, fp) !== caller.keyGeneration
  ) {
    log.warn("worker-ws", "upgrade_principal_mismatch", {
      url_fp: fp,
      jwt_fp: caller.fingerprint,
      principal_kind: principal?.kind,
    });
    signal("worker.auth_rejected", {
      reason: "principal_mismatch",
      addr,
      cooldownKey: addr ?? "worker-auth",
    });
    return new Response("unauthorized", { status: 401 });
  }
  const data: WorkerWsData = {
    kind: "worker",
    caller,
    fp,
    dashboardId: principal.dashboardId,
    authDeadlineAtMs: deps.cfg.saasMode ? caller.validUntilMs : null,
    authDeadlineTimer: null,
    conn: null,
    queue: null,
    eventRate: { startedAtMs: null, events: 0 },
    announcedChannels: createAnnouncedChannelBarrier(fp),
  };
  const ok = server.upgrade(req, {
    data,
    headers: { "Sec-WebSocket-Protocol": WORKER_AUTH_SUBPROTOCOL },
  });
  if (ok) return undefined;
  return new Response("upgrade failed", { status: 400 });
}
