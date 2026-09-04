// Connect RPC authentication resolves verified keys and tenant scope before any
// handler runs. This interceptor owns request context, move-gate leases, and
// response-aware auditing because no outer layer sees all three safely.

import {
  Code,
  ConnectError,
  createContextKey,
  type ContextValues,
  type Interceptor,
} from "@connectrpc/connect";
import type { KyselyDB } from "../db/connection.ts";
import type { JwtCache } from "../jwt.ts";
import type { CoordConfig } from "@roost/shared/config";
import type { CoordinatorMoveService } from "../coord-move/orchestrator.ts";
import type { CallerOrigin, ListenerTrust } from "../middleware/caller-origin.ts";
import { verifyJwt } from "../jwt.ts";
import {
  recordAuditTelemetry,
  shouldPersistConnectAudit,
  writeAuditLog,
} from "../middleware/security.ts";
import { signal, diag } from "@roost/shared/diag";
// Header names + auth-layer sentinels are the SPA↔coord trust contract; both
// ends must import them, never retype them.
import {
  X_ROOST_TRACE_ID,
  X_ROOST_REMOTE_ADDR,
  X_ROOST_ON_HOST,
  X_ROOST_LISTENER_TRUST,
  X_ROOST_TAB_ID,
  X_ROOST_DASHBOARD_ID,
  X_ROOST_AUTH_LAYER,
  AUTH_LAYER_DEVICE,
  AUTH_LAYER_TAILSCALE_SERVE,
  AUTH_LAYER_PUBLIC_EDGE,
} from "@roost/shared/wire/headers";
import {
  resolveCallerPrincipal,
  type AccountDeviceCaller,
  type AccountDevicePrincipal,
  type Caller,
  type LegacySelfHostedPrincipal,
  type WorkerPrincipal,
} from "./auth-principal.ts";
import {
  getDashboardAccessSnapshot,
  isDashboardActor,
  resolveDashboardActor,
  type AccessibleDashboard,
  type AccessibleOrganization,
  type DashboardAccessSnapshot,
  type DashboardActor,
  type DashboardRole,
  type OrganizationRole,
} from "./dashboard-authorization.ts";

export {
  resolveCallerPrincipal,
  type AccountDeviceCaller,
  type AccountDevicePrincipal,
  type Caller,
  type LegacySelfHostedPrincipal,
  type WorkerPrincipal,
};
export {
  getDashboardAccessSnapshot,
  isDashboardActor,
  resolveDashboardActor,
  type AccessibleDashboard,
  type AccessibleOrganization,
  type DashboardAccessSnapshot,
  type DashboardActor,
  type DashboardRole,
  type OrganizationRole,
};

// Map a Connect Code → conventional HTTP status. Audit_log records
// HTTP-semantic status so dashboards reading `WHERE status >= 400`
// stay correct across the audit-write split (interceptor for Connect,
// coord-factory for non-Connect).
function codeToHttpStatus(code: Code): number {
  switch (code) {
    case Code.InvalidArgument:
    case Code.OutOfRange: return 400;
    case Code.Unauthenticated: return 401;
    case Code.PermissionDenied: return 403;
    case Code.NotFound: return 404;
    case Code.AlreadyExists:
    case Code.Aborted: return 409;
    case Code.FailedPrecondition: return 412;
    case Code.ResourceExhausted: return 429;
    case Code.Unimplemented: return 501;
    case Code.Unavailable: return 503;
    case Code.DeadlineExceeded: return 504;
    default: return 500;
  }
}


function notFound(): never {
  throw new ConnectError("not found", Code.NotFound);
}

// Context-key for the caller. Handlers retrieve via `ctx.values.get(callerKey)`.
export const callerKey = createContextKey<Caller | null>(null);

// trace_id context key (propagated from x-roost-trace-id header).
const traceIdKey = createContextKey<string | undefined>(undefined);

// remoteAddress context key (set via x-roost-remote-addr by bun-handler).
export const remoteAddressKey = createContextKey<string | undefined>(undefined);
export const onHostKey = createContextKey<boolean>(false);
export const listenerTrustKey = createContextKey<ListenerTrust>("direct");

// Per-tab id (set via x-roost-tab-id header by SPA's connect interceptor).
// Sync terminal commands require it so concurrent same-device sockets retain
// distinct sender/viewer identities.
export const tabIdKey = createContextKey<string | undefined>(undefined);

/** Client-requested dashboard ID. It is not authority until resolved below. */
export const requestedDashboardIdKey = createContextKey<string | undefined>(undefined);
/** Server-confirmed dashboard scope, resolved exactly once by the interceptor. */
export const dashboardActorKey = createContextKey<DashboardActor | null>(null);
/** Raw bearer header preserved for retired-source relocation forwarding. */
export const authorizationKey = createContextKey<string | undefined>(undefined);

export interface AuthInterceptorDeps {
  db: KyselyDB;
  jwtCache: JwtCache;
  cfg: CoordConfig;
  move?: CoordinatorMoveService;
}


// Only durable coordinator mutations take a gate lease. Read RPCs and
// server-streaming endpoints must never hold the drain open.
const WRITE_METHODS: Record<string, true | undefined> = {
  WorkersRegister: true, WorkersHeartbeat: true, WorkersRename: true, WorkersDelete: true, WorkersDeployStart: true,
  SessionsSpawn: true, SessionsAttach: true, SessionsKill: true, SessionsRename: true,
  // SessionsInput acquires its lease only after entering the per-session FIFO.
  // Taking one here would let queued input hold the move drain open.
  SessionsCursorPos: true, SessionsAssignWorkspace: true,
  TasksEnqueue: true, TasksNextPending: true, TasksSetState: true, TasksCancel: true,
  WorkspacesCreate: true, WorkspacesUpdate: true, WorkspacesDelete: true, WorkspacesSetSessions: true,
  McpCreate: true, McpDelete: true, McpPublish: true,
  AuthMintBootstrap: true, AuthRedeemWorker: true, AuthRedeemBrowser: true,
  AuthLogout: true, AuthPasswordLogin: true, AuthOwnerActivate: true, AuthFederatedContinue: true,
  AuthPasswordAdd: true, AuthFederatedLinkBegin: true, AuthFederatedLink: true,
  AuthPasswordResetRequest: true, AuthPasswordResetRedeem: true,
  PairCreate: true, PairApprove: true, PairDeny: true,
  DevicesRevoke: true, DevicesRotateCurrent: true,
  FilesMkdir: true, TranscriptionSetConfig: true, AgentConfigSet: true,
  AttachFileChunk: true, DeleteAttachment: true,
  PushSubscribe: true, PushUnsubscribe: true,
  DiagDebugLogBatch: true,
};

// High-frequency methods whose audit rows carry no forensic signal: health
// pings, heartbeats, polling reads, and cursor chatter. Auditing them
// buried the rows that matter — DiagDebugLogBatch alone (a *receipt* for the
// SPA uploading its own debug logs, which go to disk, not this table) was 61%
// of a 7M-row, 1GB audit_log with no retention. A non-200 still gets written:
// a failing heartbeat or a rejected health probe is exactly the anomaly worth
// keeping.
const AUDIT_SKIP_METHODS: Record<string, true | undefined> = {
  AuthCoordIdentity: true, DiagDebugLogBatch: true, MiscHealth: true, WorkersHeartbeat: true,
  PairList: true, SessionsCursorPos: true,
  // Non-mutating SPA polling. These were briefly handled by the retention
  // sweep instead, which meant paying an INSERT per RPC to delete the row
  // days later. Never writing them is strictly cheaper and leaves the sweep
  // doing the one job it is actually needed for: aging out SessionsInput,
  // which IS real audit data and cannot simply be skipped.
  UiReportState: true, SessionsGetScrollbackCells: true, TranscriptionGetConfig: true,
};

export function makeAuthInterceptor(deps: AuthInterceptorDeps): Interceptor {
  // Managed startup serves identity and rejects reconnecting legacy clients
  // before the owner transaction can assign their rows to a dashboard. Audit
  // only after that transaction creates the first account; otherwise the
  // preservation oracle is polluted with unscoped transition traffic.
  let managedBootstrapComplete = !deps.cfg.saasMode;
  return (next) => async (req) => {
    let auditEnabledForThisRequest = managedBootstrapComplete;
    if (!auditEnabledForThisRequest) {
      try {
        auditEnabledForThisRequest = Boolean(
          await deps.db.selectFrom("accounts").select("id").limit(1).executeTakeFirst(),
        );
        if (auditEnabledForThisRequest) managedBootstrapComplete = true;
      } catch {
        // Startup migration/locking failures must not turn a successful RPC
        // into a 500. The next request retries the bootstrap-state read.
      }
    }
    let caller: Caller | null = null;
    // Per-RPC record lives in audit_log (finally below) — method/path/
    // status/trace_id/caller_fp; the OTel span wrapper was retired
    // (PONYTAIL-BORDERLINE B1, no exporter was ever configured).
    const method = req.method?.name ?? "unknown";
    const service = req.service?.typeName ?? "unknown";
    const path = `/${service}/${method}`;
    const auth = req.header.get("authorization");
    if (auth?.startsWith("Bearer ")) {
      const token = auth.slice(7);
      try {
        const verified = await verifyJwt(token, {
          db: deps.db,
          cache: deps.jwtCache,
          jwtMaxAgeSecs: deps.cfg.jwtMaxAgeSecs,
        });
        caller = await resolveCallerPrincipal(deps.db, deps.cfg, verified);
      } catch {
        diag("auth.jwt_verify_failed", { path });
        // leave caller null; authenticated handlers reject
      }
    }
    const requestedDashboardId = req.header.get(X_ROOST_DASHBOARD_ID)?.trim() || undefined;
    // A selected dashboard is a request, never a claim. Resolve it after JWT
    // verification and before any tenant handler can read a resource.
    const actor = caller?.kind === "account-device" && requestedDashboardId
      ? await resolveDashboardActor(deps.db, caller.fingerprint, requestedDashboardId)
      : null;
    const traceId = req.header.get(X_ROOST_TRACE_ID) ?? undefined;
    req.contextValues.set(callerKey, caller);
    req.contextValues.set(dashboardActorKey, actor);
    req.contextValues.set(requestedDashboardIdKey, requestedDashboardId);
    req.contextValues.set(traceIdKey, traceId);
    req.contextValues.set(remoteAddressKey, req.header.get(X_ROOST_REMOTE_ADDR) ?? undefined);
    req.contextValues.set(onHostKey, req.header.get(X_ROOST_ON_HOST) === "1");
    const listenerHeader = req.header.get(X_ROOST_LISTENER_TRUST);
    const listenerTrust: ListenerTrust =
      listenerHeader === AUTH_LAYER_TAILSCALE_SERVE || listenerHeader === AUTH_LAYER_PUBLIC_EDGE
        ? listenerHeader
        : "direct";
    req.contextValues.set(listenerTrustKey, listenerTrust);
    req.contextValues.set(tabIdKey, req.header.get(X_ROOST_TAB_ID) ?? undefined);
    req.contextValues.set(authorizationKey, auth);
    let status = 200;
    const lease = deps.move && WRITE_METHODS[method] ? deps.move.gate.acquire() : null;
    try {
      return await next(req);
    } catch (e) {
      status = e instanceof ConnectError ? codeToHttpStatus(e.code) : 500;
      throw e;
    } finally {
      lease?.release();
      recordAuditTelemetry(path, status);
      if (
        auditEnabledForThisRequest
        && (status !== 200 || !AUDIT_SKIP_METHODS[method])
        && shouldPersistConnectAudit({
          listener: listenerTrust,
          status,
          callerFp: caller?.fingerprint ?? null,
        })
      ) {
        writeAuditLog({
          db: deps.db,
          status,
          method: "POST",
          path,
          traceId,
          callerFp: caller?.fingerprint ?? null,
          dashboardId: actor?.dashboardId
            ?? (caller?.kind === "worker" ? caller.dashboardId : undefined)
            ?? (deps.cfg.managedContainer ? deps.cfg.instanceId : undefined),
          recordTelemetry: false,
        });
      }
      if (status === 401) {
        signal("auth.rpc_rejected", {
          path,
          reason: caller ? "forbidden" : "no_caller",
          caller_fp: caller?.fingerprint ?? null,
          cooldownKey: caller?.fingerprint ?? "anon",
        });
      }
    }
  };
}
export function callerOrigin(values: ContextValues): CallerOrigin {
  return {
    clientIp: values.get(remoteAddressKey) ?? "unknown",
    onHost: values.get(onHostKey),
    listener: values.get(listenerTrustKey),
  };
}

function authenticationRequired(): never {
  throw new ConnectError(
    "authentication required",
    Code.Unauthenticated,
    new Headers({ [X_ROOST_AUTH_LAYER]: AUTH_LAYER_DEVICE }),
  );
}

/** Requires browser authority; worker principals are never browser devices. */
export function requireAccountDevice(values: ContextValues): AccountDeviceCaller {
  const caller = values.get(callerKey);
  if (caller?.kind === "account-device" || caller?.kind === "legacy-self-hosted") {
    return caller;
  }
  return authenticationRequired();
}

/** Requires a persisted worker with a non-null dashboard assignment. */
export function requireWorker(values: ContextValues): WorkerPrincipal {
  const caller = values.get(callerKey);
  if (caller?.kind === "worker") return caller;
  return authenticationRequired();
}


export function requestedDashboardId(values: ContextValues): string | undefined {
  return values.get(requestedDashboardIdKey);
}

/** Requires a browser principal and an active selected dashboard membership. */
export function requireDashboardActor(values: ContextValues): DashboardActor {
  const caller = requireAccountDevice(values);
  const actor = values.get(dashboardActorKey);
  if (
    isDashboardActor(actor)
    && actor.deviceFingerprint === caller.fingerprint
    && (caller.kind !== "account-device" || actor.accountId === caller.accountId)
  ) {
    return actor;
  }
  return notFound();
}

/** Dashboard admins manage dashboard resources and configuration. */
export function requireDashboardAdmin(values: ContextValues): DashboardActor {
  const actor = requireDashboardActor(values);
  if (actor.dashboardRole !== "admin") {
    throw new ConnectError("dashboard admin required", Code.PermissionDenied);
  }
  return actor;
}

/** Organization owners and admins manage organization-level resources. */
export function requireOrganizationAdmin(values: ContextValues): DashboardActor {
  const actor = requireDashboardActor(values);
  if (actor.organizationRole !== "owner" && actor.organizationRole !== "admin") {
    throw new ConnectError("organization admin required", Code.PermissionDenied);
  }
  return actor;
}

/** Account-device caller if present, else null for on-host recovery paths. */
export function optionalAccountDevice(values: ContextValues): AccountDeviceCaller | null {
  const caller = values.get(callerKey);
  if (!caller) return null;
  return requireAccountDevice(values);
}
