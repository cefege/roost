// Session RPC handlers: list/spawn/attach/kill/rename/input/cursor-pos/
// assign-workspace. Most forward a browser-command frame to the session's
// worker and await its reply. Terminal views and resize aggregation are handled
// only on the socket-bound TerminalViewHub path. Scrollback reads live in
// handlers-sessions-scrollback.ts.

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  CoordinatorService,
  SessionsListResponseSchema, SessionsAttachResponseSchema,
  SessionsKillResponseSchema, SessionsRenameResponseSchema,
  SessionsInputResponseSchema, SessionsCursorPosResponseSchema,
  SessionsAssignWorkspaceResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { sessionToProto } from "@roost/shared/wire/session-proto";
import { asSessionId, asWorkspaceId, SessionStatus } from "@roost/shared/wire";
import type { SessionStatus as SessionStatusValue } from "@roost/shared/wire";
import { safeJsonParse } from "@roost/shared/json";
import type { ClientControlFrame } from "@roost/shared/wire";
import { log } from "@roost/shared/log";
import type { KyselyDB } from "../db/connection.ts";
import { appendEvent, SESSION_COLUMNS } from "../event-log.ts";
import { workspaceBus } from "../buses.ts";
import { publishPresence } from "../presence-hub.ts";
import {
  callerKey,
  requireAccountDevice,
  requireDashboardActor,
  requireDashboardAdmin,
  requireWorker,
  tabIdKey,
  remoteAddressKey,
} from "./auth-interceptor.ts";
import { createPendingRpc } from "../router/pending-rpcs.ts";
import { getWorkerHubSocket } from "./worker-service.ts";
import { sendBrowserCmd, forwardToSessionWorker, requireSessionWorkerSocket } from "./router-helpers.ts";
import type { ConnectDeps } from "./router.ts";
import { makeSessionScrollbackHandlers } from "./handlers-sessions-scrollback.ts";
import { handleSessionsSpawn } from "./handler-session-spawn.ts";
import { bindSyncSessionSnapshot } from "./sync-snapshot-registry.ts";
import {
  nextCompatibilityInputSeq,
  processInputControl,
  terminalViewerIdentity,
} from "./session-control.ts";

// Proto arrives as a bare string; the projection and the SPA both speak the
// shared SessionStatus union. Narrow once at the boundary.
function sessionStatusOf(raw: string): SessionStatusValue {
  const parsed = SessionStatus.safeParse(raw);
  if (!parsed.success) {
    throw new ConnectError(`invalid session status ${JSON.stringify(raw)}`, Code.InvalidArgument);
  }
  return parsed.data;
}

// DB row → proto Session through the shared terminal-session adapter.
function sessionRowToProto(row: any) {
  return sessionToProto({
    id: row.id,
    worker_fp: row.worker_fp,
    channel: row.channel,
    kind: row.kind,
    cwd: row.cwd,
    workspace_id: row.workspace_id ?? null,
    status: row.status,
    created_at: row.created_at,
    closed_at: row.closed_at ?? null,
    custom_title: row.custom_title ?? null,
    git_branch: row.git_branch ?? null,
    git_remote: row.git_remote ?? null,
    pr_number: row.pr_number ?? null,
    pr_state: (row.pr_state ?? null) as never,
    pr_checks: (row.pr_checks ?? null) as never,
    pr_url: row.pr_url ?? null,
    ports: row.ports_json ? safeJsonParse<number[]>(row.ports_json, [], "session.ports") : [],
    spawn_cwd: row.spawn_cwd ?? null,
  });
}

type SessionMethods =
  | "sessionsList" | "sessionsSpawn" | "sessionsAttach" | "sessionsKill"
  | "sessionsRename" | "sessionsInput" | "sessionsCursorPos"
  | "sessionsAssignWorkspace"
  | "sessionsGetScrollbackCells" | "sessionsSearchScrollback";

export function makeSessionHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, SessionMethods> {
  return {
    async sessionsList(req, ctx) {
      const principal = ctx.values.get(callerKey);
      let dashboardId: string;
      let snapshotCallerFingerprint: string | null = null;
      if (principal?.kind === "worker") {
        const worker = requireWorker(ctx.values);
        if (
          req.workerFp !== worker.fingerprint
          || req.status !== "open"
          || req.syncSocketId !== undefined
        ) {
          throw new ConnectError(
            "worker session listing is restricted to its own open sessions",
            Code.PermissionDenied,
          );
        }
        dashboardId = worker.dashboardId;
      } else {
        const actor = requireDashboardActor(ctx.values);
        const caller = requireAccountDevice(ctx.values);
        dashboardId = actor.dashboardId;
        snapshotCallerFingerprint = caller.fingerprint;
      }
      let q = deps.db.selectFrom("sessions")
        .select([...SESSION_COLUMNS])
        .where("dashboard_id", "=", dashboardId);
      const status = req.status || "open";
      if (req.workerFp) q = q.where("worker_fp", "=", req.workerFp);
      if (status !== "all") q = q.where("status", "=", sessionStatusOf(status));
      const rows = await q.execute();
      const syncSnapshotToken = req.syncSocketId && snapshotCallerFingerprint
        ? bindSyncSessionSnapshot(
          req.syncSocketId,
          snapshotCallerFingerprint,
          dashboardId,
          rows.map((row) => row.id),
        )
        : null;
      return create(SessionsListResponseSchema, {
        sessions: rows.map(sessionRowToProto),
        syncSnapshotToken: syncSnapshotToken ?? undefined,
      });
    },

    async sessionsSpawn(req, ctx) {
      const actor = requireDashboardAdmin(ctx.values);
      return handleSessionsSpawn(
        deps,
        req,
        requireAccountDevice(ctx.values),
        actor,
        ctx.values.get(tabIdKey),
      );
    },

    async sessionsAttach(req, ctx) {
      const actor = requireDashboardActor(ctx.values);
      const caller = requireAccountDevice(ctx.values);
      const { row, sock } = await requireSessionWorkerSocket(deps.db, actor, req.sessionId);
      const pending = createPendingRpc<{ replay_offset: number }>(undefined, row.worker_fp);
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "attach" as const,
        session_id: asSessionId(req.sessionId),
        ...(req.fromOffset !== undefined ? { from_offset: Number(req.fromOffset) } : {}),
      });
      const data = await pending.promise;
      return create(SessionsAttachResponseSchema, { replayOffset: BigInt(data.replay_offset) });
    },

    async sessionsKill(req, ctx) {
      const actor = requireDashboardAdmin(ctx.values);
      const caller = requireAccountDevice(ctx.values);
      const row = await deps.db.selectFrom("sessions")
        .select(["worker_fp"])
        .where("id", "=", req.sessionId)
        .where("dashboard_id", "=", actor.dashboardId)
        .executeTakeFirst();
      if (!row) return create(SessionsKillResponseSchema, { accepted: false });
      const sock = getWorkerHubSocket(row.worker_fp);
      if (!sock) {
        // Worker offline (e.g. lid-closed Mac): a normal kill needs a worker
        // ack we'll never get. force=true tombstones in coord — append a
        // `closed` event (foldEvent deletes the row + cascades + publishes →
        // sidebar drops it). session ids are never reused, so this closed
        // event is a permanent tombstone: if that worker ever returns with the
        // PTY still live, the snapshot reconcile (event-log.ts) detects the
        // orphan and reaps it. Non-force stays accepted:false so a transient
        // disconnect never nukes a real session.
        if (req.force) {
          await appendEvent(deps.db, {
            kind: "closed" as const, session_id: asSessionId(req.sessionId),
            exit_code: null, ts: Date.now(),
          }, {
            worker_fp: null,
            client_seq: null,
            dashboardId: actor.dashboardId,
          });
          log.info("connect-router.sessionsKill", "force_closed_offline_worker", {
            session_id: req.sessionId, worker_fp: row.worker_fp,
          });
          return create(SessionsKillResponseSchema, { accepted: true });
        }
        return create(SessionsKillResponseSchema, { accepted: false });
      }
      try {
        sendBrowserCmd(sock, caller, randomUUID(), {
          kind: "kill" as const, session_id: asSessionId(req.sessionId),
        });
        return create(SessionsKillResponseSchema, { accepted: true });
      } catch (error) {
        log.warn("connect-router.sessionsKill", "kill_send_failed", {
          session_id: req.sessionId, error: String(error),
        });
        return create(SessionsKillResponseSchema, { accepted: false });
      }
    },

    async sessionsRename(req, ctx) {
      const actor = requireDashboardAdmin(ctx.values);
      // Append a `renamed` event → event-log folds custom_title into the
      // sessions projection + publishes on sessionBus → every SPA updates live.
      // "" clears the override (revert to auto title). Capped so a runaway
      // paste can't bloat the row. Returns ok:false if the session is gone.
      const exists = await deps.db.selectFrom("sessions").select(["id"])
        .where("id", "=", req.sessionId)
        .where("dashboard_id", "=", actor.dashboardId)
        .executeTakeFirst();
      if (!exists) return create(SessionsRenameResponseSchema, { ok: false });
      await appendEvent(deps.db, {
        kind: "renamed" as const,
        session_id: asSessionId(req.sessionId),
        custom_title: req.title.trim().slice(0, 200),
        ts: Date.now(),
      }, {
        worker_fp: null,
        client_seq: null,
        dashboardId: actor.dashboardId,
      });
      log.info("connect-router.sessionsRename", "renamed", {
        session_id: req.sessionId, cleared: req.title.trim() === "",
      });
      return create(SessionsRenameResponseSchema, { ok: true });
    },



    async sessionsInput(req, ctx) {
      const actor = requireDashboardActor(ctx.values);
      const caller = requireAccountDevice(ctx.values);
      const result = await processInputControl(deps, {
        identity: terminalViewerIdentity(
          caller.fingerprint,
          ctx.values.get(tabIdKey),
          ctx.values.get(remoteAddressKey) ?? undefined,
          actor.dashboardId,
        ),
        sessionId: req.sessionId,
        inputSeq: nextCompatibilityInputSeq(),
        data: req.data,
      });
      return create(SessionsInputResponseSchema, {
        accepted: result.status === "accepted",
      });
    },

    async sessionsCursorPos(req, ctx) {
      const actor = requireDashboardActor(ctx.values);
      const caller = requireAccountDevice(ctx.values);
      const tabId = ctx.values.get(tabIdKey);
      const viewerKey = tabId ? `${caller.fingerprint}:${tabId}` : caller.fingerprint;
      const row = await deps.db.selectFrom("sessions")
        .select(["worker_fp", "channel"])
        .where("id", "=", req.sessionId)
        .where("dashboard_id", "=", actor.dashboardId)
        .executeTakeFirst();
      if (row) {
        publishPresence(row.worker_fp, row.channel, {
          kind: "presence-delta",
          channel_id: row.channel,
          viewer_id: viewerKey,
          cursor_col: req.col, cursor_row: req.row,
          label: caller.fingerprint.slice(0, 8),
        });
      }
      const ok = await forwardToSessionWorker(deps.db, actor, req.sessionId, caller, {
        kind: "cursor-pos" as const,
        session_id: asSessionId(req.sessionId),
        col: req.col, row: req.row,
      } as ClientControlFrame);
      return create(SessionsCursorPosResponseSchema, { accepted: ok });
    },

    async sessionsAssignWorkspace(req, ctx) {
      const actor = requireDashboardAdmin(ctx.values);
      const sessionId = req.sessionId;
      const targetWs = req.workspaceId || null;
      const session = await deps.db.selectFrom("sessions").select("id")
        .where("id", "=", sessionId)
        .where("dashboard_id", "=", actor.dashboardId)
        .executeTakeFirst();
      if (!session) return create(SessionsAssignWorkspaceResponseSchema, { ok: false });
      if (targetWs) {
        const target = await deps.db.selectFrom("workspaces").select("id")
          .where("id", "=", targetWs)
          .where("dashboard_id", "=", actor.dashboardId)
          .executeTakeFirst();
        if (!target) return create(SessionsAssignWorkspaceResponseSchema, { ok: false });
      }
      // B5: session→workspace membership has TWO representations the SPA both
      // reads — sessions.workspace_id (column; AllView/MachineSection/SessionRow)
      // and the workspace_sessions junction (orphanSessions/sessionsForWorkspace).
      // This path historically wrote ONLY the column, so a spawn-assigned session
      // was missing from the junction → orphanSessions double-counted it (shown
      // under its workspace AND Unassigned). Maintain BOTH so all readers agree.
      const touched = new Set<string>();
      if (targetWs) touched.add(targetWs);
      await appendEvent(deps.db, {
        kind: "workspace_assigned",
        session_id: asSessionId(sessionId),
        workspace_id: targetWs ? asWorkspaceId(targetWs) : null,
        ts: Date.now(),
      }, {
        worker_fp: null,
        client_seq: null,
        dashboardId: actor.dashboardId,
        extraWork: async (trx) => {
          if (targetWs) {
            const target = await trx.selectFrom("workspaces").select("id")
              .where("id", "=", targetWs)
              .where("dashboard_id", "=", actor.dashboardId)
              .executeTakeFirst();
            if (!target) throw new ConnectError("not found", Code.NotFound);
          }
          const prior = (await trx.selectFrom("workspace_sessions").select("workspace_id")
            .where("session_id", "=", sessionId)
            .where("dashboard_id", "=", actor.dashboardId)
            .execute()).map(r => r.workspace_id as string);
          for (const wsId of prior) touched.add(wsId);
        },
      });
      // Publish the updated junction for every touched workspace so SPA
      // junction-readers update live — strictly AFTER commit (README invariant:
      // no bus delta may precede the durable state it describes).
      for (const wsId of touched) {
        const ws = await deps.db.selectFrom("workspaces").select("version")
          .where("id", "=", wsId)
          .where("dashboard_id", "=", actor.dashboardId)
          .executeTakeFirst();
        if (!ws) continue;
        const sids = (await deps.db.selectFrom("workspace_sessions").select("session_id")
          .where("workspace_id", "=", wsId)
          .where("dashboard_id", "=", actor.dashboardId)
          .execute()).map(r => asSessionId(r.session_id));
        workspaceBus.publish({
          kind: "sessions-set",
          id: asWorkspaceId(wsId),
          session_ids: sids,
          version: ws.version,
          _dashboard_id: actor.dashboardId,
        });
      }
      return create(SessionsAssignWorkspaceResponseSchema, { ok: true });
    },

    ...makeSessionScrollbackHandlers(deps),
  };
}
