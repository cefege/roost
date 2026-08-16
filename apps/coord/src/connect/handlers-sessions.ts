// Session RPC handlers: list/spawn/attach/kill/rename/resize/input/cursor-pos/
// assign-workspace. Most forward a browser-command frame to the session's worker
// hub socket (sendBrowserCmd / forwardToSessionWorker) and await the worker
// reply; resize also bumps the viewer-presence tracker. The two scrollback reads
// live in handlers-sessions-scrollback.ts and are spread in below; the whole
// object spreads into router.ts's single router.service() literal (400-line cap).

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  CoordinatorService,
  SessionsListResponseSchema, SessionsAttachResponseSchema,
  SessionsKillResponseSchema, SessionsRenameResponseSchema, SessionsResizeResponseSchema,
  SessionsInputResponseSchema, SessionsCursorPosResponseSchema,
  SessionsAssignWorkspaceResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { sessionToProto } from "@roost/shared/wire/session-proto";
import { asSessionId, asWorkspaceId } from "@roost/shared/wire";
import { safeJsonParse } from "@roost/shared/json";
import type { ClientControlFrame } from "@roost/shared/wire";
import { log } from "@roost/shared/log";
import type { KyselyDB } from "../db/connection.ts";
import { appendEvent } from "../event-log.ts";
import { workspaceBus } from "../buses.ts";
import { requireAuth, tabIdKey, remoteAddressKey } from "./auth-interceptor.ts";
import { getWorkerHubSocket } from "./worker-service.ts";
import { createPendingRpc } from "../router/pending-rpcs.ts";
import { sendBrowserCmd, forwardToSessionWorker } from "./router-helpers.ts";
import type { ConnectDeps } from "./router.ts";
import { makeSessionScrollbackHandlers } from "./handlers-sessions-scrollback.ts";
import { handleSessionsSpawn } from "./handler-session-spawn.ts";
import { bindSyncSessionSnapshot } from "./sync-snapshot-registry.ts";
import {
  nextCompatibilityInputSeq,
  processInputControl,
  processViewportControl,
  terminalViewerIdentity,
} from "./session-control.ts";


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
  | "sessionsRename" | "sessionsResize" | "sessionsInput"
  | "sessionsCursorPos" | "sessionsAssignWorkspace"
  | "sessionsGetScrollbackCells" | "sessionsSearchScrollback";

export function makeSessionHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, SessionMethods> {
  return {
    async sessionsList(req, ctx) {
      const caller = requireAuth(ctx.values);
      let q = deps.db.selectFrom("sessions").select([
        "id", "worker_fp", "channel", "kind", "cwd", "workspace_id", "status",
        "created_at", "closed_at", "custom_title", "git_branch", "git_remote",
        "pr_number", "pr_state", "pr_checks", "pr_url", "ports_json", "spawn_cwd",
      ]);
      if (req.workerFp) q = q.where("worker_fp", "=", req.workerFp);
      const status = req.status || "open";
      if (status !== "all") q = q.where("status", "=", status as any);
      const rows = await q.execute();
      const syncSnapshotToken = req.syncSocketId
        ? bindSyncSessionSnapshot(req.syncSocketId, caller.fingerprint, rows.map((row) => row.id))
        : null;
      return create(SessionsListResponseSchema, {
        sessions: rows.map(sessionRowToProto),
        syncSnapshotToken: syncSnapshotToken ?? undefined,
      });
    },

    async sessionsSpawn(req, ctx) {
      return handleSessionsSpawn(
        deps,
        req,
        requireAuth(ctx.values),
        ctx.values.get(tabIdKey),
      );
    },

    async sessionsAttach(req, ctx) {
      const caller = requireAuth(ctx.values);
      const row = await deps.db.selectFrom("sessions").select(["worker_fp"]).where("id", "=", req.sessionId).executeTakeFirst();
      if (!row) throw new ConnectError("unknown session", Code.NotFound);
      const sock = getWorkerHubSocket(row.worker_fp);
      if (!sock) throw new ConnectError("worker not connected", Code.Unavailable);
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
      const caller = requireAuth(ctx.values);
      const row = await deps.db.selectFrom("sessions").select(["worker_fp"]).where("id", "=", req.sessionId).executeTakeFirst();
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
      } catch {
        return create(SessionsKillResponseSchema, { accepted: false });
      }
    },

    async sessionsRename(req, ctx) {
      requireAuth(ctx.values);
      // Append a `renamed` event → event-log folds custom_title into the
      // sessions projection + publishes on sessionBus → every SPA updates live.
      // "" clears the override (revert to auto title). Capped so a runaway
      // paste can't bloat the row. Returns ok:false if the session is gone.
      const exists = await deps.db.selectFrom("sessions").select(["id"])
        .where("id", "=", req.sessionId).executeTakeFirst();
      if (!exists) return create(SessionsRenameResponseSchema, { ok: false });
      await appendEvent(deps.db, {
        kind: "renamed" as const,
        session_id: asSessionId(req.sessionId),
        custom_title: req.title.trim().slice(0, 200),
        ts: Date.now(),
      });
      log.info("connect-router.sessionsRename", "renamed", {
        session_id: req.sessionId, cleared: req.title.trim() === "",
      });
      return create(SessionsRenameResponseSchema, { ok: true });
    },

    async sessionsResize(req, ctx) {
      const caller = requireAuth(ctx.values);
      const result = await processViewportControl(deps, {
        identity: terminalViewerIdentity(
          caller.fingerprint,
          ctx.values.get(tabIdKey),
          ctx.values.get(remoteAddressKey) ?? undefined,
        ),
        sessionId: req.sessionId,
        clientSeq: req.clientSeq,
        cols: req.cols,
        rows: req.rows,
        cause: req.cause,
        heldCellSeq: BigInt(req.heldCellSeq),
      });
      return create(SessionsResizeResponseSchema, {
        accepted: result.status === "accepted",
      });
    },


    async sessionsInput(req, ctx) {
      const caller = requireAuth(ctx.values);
      const result = await processInputControl(deps, {
        identity: terminalViewerIdentity(
          caller.fingerprint,
          ctx.values.get(tabIdKey),
          ctx.values.get(remoteAddressKey) ?? undefined,
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
      const caller = requireAuth(ctx.values);
      const tabId = ctx.values.get(tabIdKey);
      const viewerKey = tabId ? `${caller.fingerprint}:${tabId}` : caller.fingerprint;
      const row = await deps.db.selectFrom("sessions").select(["worker_fp", "channel"]).where("id", "=", req.sessionId).executeTakeFirst();
      if (row) {
        const { publishPresence } = await import("../presence-hub.ts");
        publishPresence(row.worker_fp, row.channel, {
          kind: "presence-delta",
          channel_id: row.channel,
          viewer_id: viewerKey,
          cursor_col: req.col, cursor_row: req.row,
          label: caller.fingerprint.slice(0, 8),
        });
      }
      const ok = await forwardToSessionWorker(deps.db, req.sessionId, caller, {
        kind: "cursor-pos" as const,
        session_id: asSessionId(req.sessionId),
        col: req.col, row: req.row,
      } as ClientControlFrame, viewerKey);
      return create(SessionsCursorPosResponseSchema, { accepted: ok });
    },

    async sessionsAssignWorkspace(req, ctx) {
      requireAuth(ctx.values);
      const sessionId = req.sessionId;
      const targetWs = req.workspaceId || null;
      // B5: session→workspace membership has TWO representations the SPA both
      // reads — sessions.workspace_id (column; AllView/MachineSection/SessionRow)
      // and the workspace_sessions junction (orphanSessions/sessionsForWorkspace).
      // This path historically wrote ONLY the column, so a spawn-assigned session
      // was missing from the junction → orphanSessions double-counted it (shown
      // under its workspace AND Unassigned). Maintain BOTH so all readers agree.
      // (1) column, via the workspace_assigned event (SPA column delta):
      await appendEvent(deps.db, {
        kind: "workspace_assigned",
        session_id: asSessionId(sessionId),
        workspace_id: targetWs ? asWorkspaceId(targetWs) : null,
        ts: Date.now(),
      });
      // (2) junction: move the session to the target (remove from any prior).
      const now = Date.now();
      const touched = await deps.db.transaction().execute(async (trx) => {
        const prior = (await trx.selectFrom("workspace_sessions").select("workspace_id")
          .where("session_id", "=", sessionId).execute()).map(r => r.workspace_id as string);
        await trx.deleteFrom("workspace_sessions").where("session_id", "=", sessionId).execute();
        if (targetWs) {
          await trx.insertInto("workspace_sessions")
            .values({ workspace_id: targetWs, session_id: sessionId, added_at_ms: now })
            .onConflict((oc) => oc.columns(["workspace_id", "session_id"]).doNothing())
            .execute();
        }
        return [...new Set([...prior, ...(targetWs ? [targetWs] : [])])];
      });
      // Publish the updated junction for every touched workspace so SPA
      // junction-readers update live (not just on reload).
      for (const wsId of touched) {
        const ws = await deps.db.selectFrom("workspaces").select("version").where("id", "=", wsId).executeTakeFirst();
        if (!ws) continue;
        const sids = (await deps.db.selectFrom("workspace_sessions").select("session_id")
          .where("workspace_id", "=", wsId).execute()).map(r => r.session_id as any);
        workspaceBus.publish({ kind: "sessions-set", id: wsId as any, session_ids: sids, version: ws.version });
      }
      return create(SessionsAssignWorkspaceResponseSchema, { ok: true });
    },

    ...makeSessionScrollbackHandlers(deps),
  };
}
