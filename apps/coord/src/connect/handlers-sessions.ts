// Session RPC handlers: list/spawn/attach/kill/resize/user-message/input/
// cursor-pos/assign-workspace/get-scrollback-since/get-scrollback-cells.
// Most forward a browser-command frame to the session's worker hub socket
// (sendBrowserCmd / _forwardSimple) and await the worker reply; resize also
// bumps the viewer-presence tracker. Spread into router.ts's single
// router.service() literal. Split out of router.ts (400-line cap).

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { randomUUID, createHash } from "node:crypto";
import {
  CoordinatorService,
  SessionsListResponseSchema, SessionsSpawnResponseSchema, SessionsAttachResponseSchema,
  SessionsKillResponseSchema, SessionsRenameResponseSchema, SessionsResizeResponseSchema, SessionsUserMessageResponseSchema,
  SessionsInputResponseSchema, SessionsCursorPosResponseSchema,
  SessionsAssignWorkspaceResponseSchema,
  SessionsGetScrollbackCellsResponseSchema,
  SessionsGetChatHistoryResponseSchema, SessionsGetChatBlockResponseSchema,
  SessionsChatCommandResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { sessionToProto } from "@roost/shared/wire/agent-proto";
import { cellRowToProto } from "@roost/shared/cell/cell-proto";
import { chatMessageToProto } from "@roost/shared/chat/wire";
import type { ChatMessage } from "@roost/shared/chat/wire";
import type { CellRow } from "@roost/shared/cell";
import { asSessionId, asWorkspaceId, ClaudeMode } from "@roost/shared/wire";
import { safeJsonParse } from "@roost/shared/json";
import type { ClientControlFrame } from "@roost/shared/wire";
import { log } from "@roost/shared/log";
import { diag } from "@roost/shared/diag";
import type { KyselyDB } from "../db/connection.ts";
import { appendEvent } from "../event-log.ts";
import { workspaceBus } from "../buses.ts";
import { requireAuth, tabIdKey, remoteAddressKey } from "./auth-interceptor.ts";
import type { Caller } from "./auth-interceptor.ts";
import { getWorkerHubSocket } from "./worker-service.ts";
import { getCachedSessionWorker, cacheSessionWorker } from "../byte-hub.ts";
import { createPendingRpc } from "../router/pending-rpcs.ts";
import { sendBrowserCmd } from "./router-helpers.ts";
import { _bumpViewer } from "./viewer-tracker.ts";
import type { ConnectDeps } from "./router.ts";

const _coordSha8 = (b: Uint8Array): string =>
  createHash("sha256").update(b).digest("hex").slice(0, 8);

// DB row → proto Session. Defers to the shared sessionToProto so every
// Zod-AgentState → proto mapping lives in one place; malformed agent_json
// degrades to agent=null rather than crashing SessionsList.
function sessionRowToProto(row: any) {
  let agent: unknown = null;
  if (row.agent_json) {
    try { agent = JSON.parse(row.agent_json); }
    catch (e) {
      log.warn("router", "agent_json_parse_failed", {
        session_id: row.id, error: (e as Error).message,
      });
    }
  }
  return sessionToProto({
    id: row.id,
    worker_fp: row.worker_fp,
    channel: row.channel,
    kind: row.kind,
    cwd: row.cwd,
    workspace_id: row.workspace_id ?? null,
    status: row.status,
    agent: agent as never,
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

// Resolve a session's worker and forward a control frame, fire-and-ack.
// Returns false (never throws) when the session/worker is gone so the
// handlers can report accepted:false.
async function _forwardSimple(
  db: KyselyDB,
  sessionIdRaw: string,
  caller: Caller,
  frame: ClientControlFrame,
  overrideViewerId?: string,
): Promise<boolean> {
  const row = await db.selectFrom("sessions").select(["worker_fp"]).where("id", "=", sessionIdRaw).executeTakeFirst();
  if (!row) return false;
  const sock = getWorkerHubSocket(row.worker_fp);
  if (!sock) return false;
  try { sendBrowserCmd(sock, caller, randomUUID(), frame, overrideViewerId); return true; }
  catch (e) { log.warn("connect-router._forwardSimple", "send_failed", { error: String(e) }); return false; }
}

type SessionMethods =
  | "sessionsList" | "sessionsSpawn" | "sessionsAttach" | "sessionsKill"
  | "sessionsRename" | "sessionsResize" | "sessionsUserMessage" | "sessionsInput"
  | "sessionsCursorPos" | "sessionsAssignWorkspace"
  | "sessionsGetScrollbackCells"
  | "sessionsGetChatHistory" | "sessionsGetChatBlock" | "sessionsChatCommand";

export function makeSessionHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, SessionMethods> {
  return {
    async sessionsList(req, ctx) {
      requireAuth(ctx.values);
      let q = deps.db.selectFrom("sessions").selectAll();
      if (req.workerFp) q = q.where("worker_fp", "=", req.workerFp);
      const status = req.status || "open";
      if (status !== "all") q = q.where("status", "=", status as any);
      const rows = await q.execute();
      return create(SessionsListResponseSchema, { sessions: rows.map(sessionRowToProto) });
    },

    async sessionsSpawn(req, ctx) {
      const caller = requireAuth(ctx.values);
      const sock = getWorkerHubSocket(req.workerFp);
      if (!sock) throw new ConnectError(`worker ${req.workerFp.slice(0, 12)} not connected`, Code.FailedPrecondition);
      const pending = createPendingRpc<{ session_id: string; channel_id: number }>(15_000, req.workerFp);
      const frame: ClientControlFrame = req.kind === "shell"
        ? { kind: "spawn-shell", folder: req.folder, cols: req.cols, rows: req.rows, ...(req.sessionId ? { session_id: asSessionId(req.sessionId) } : {}) }
        : { kind: "spawn-claude", folder: req.folder, initial_mode: ClaudeMode.parse(req.initialMode ?? "default"), cols: req.cols, rows: req.rows, ...(req.sessionId ? { session_id: asSessionId(req.sessionId) } : {}) };
      sendBrowserCmd(sock, caller, pending.request_id, frame);
      const data = await pending.promise;
      return create(SessionsSpawnResponseSchema, {
        sessionId: data.session_id,
        channelId: data.channel_id,
      });
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
      // Per-tab id from x-roost-tab-id header. Two tabs from the same
      // browser have the same caller.fingerprint but different tab_id;
      // composing them gives the viewport-claim maps (coord + worker)
      // a per-tab key so one tab's withdraw doesn't kill the other
      // tab's claim. tab_id is null only for non-SPA callers (none
      // currently invoke sessionsResize).
      const tabId = ctx.values.get(tabIdKey);
      const viewerKey = tabId ? `${caller.fingerprint}:${tabId}` : caller.fingerprint;
      // Forward first; bump the viewer map only if the worker accepted.
      // Previously the bump was unconditional → phantom dots persisted
      // for 60s against killed sessions / offline workers. Withdraws
      // (cols=0||rows=0) still update the map even when the forward
      // fails so a closing tab clears its dot even mid-worker-bounce.
      // client_seq is uint64 on the wire (bigint) but the Zod
      // ClientControlFrame uses plain number — values comfortably fit in
      // Number.MAX_SAFE_INTEGER for a monotonic per-window counter (would
      // need ~285 years at 1 kHz). Convert via Number(); 0 means "unset".
      const clientSeq = req.clientSeq ? Number(req.clientSeq) : undefined;
      const ok = await _forwardSimple(deps.db, req.sessionId, caller, {
        kind: "resize" as const,
        session_id: asSessionId(req.sessionId),
        cols: req.cols, rows: req.rows,
        client_seq: clientSeq,
        cause: req.cause || undefined, // numeric ResizeCause; 0/unset → omit
      } as ClientControlFrame, viewerKey);
      const isWithdraw = req.cols <= 0 || req.rows <= 0;
      if (ok || isWithdraw) {
        const clientIp = ctx.values.get(remoteAddressKey) ?? undefined;
        _bumpViewer(req.sessionId, viewerKey, req.cols, req.rows, clientSeq, clientIp);
      }
      return create(SessionsResizeResponseSchema, { accepted: ok });
    },

    async sessionsUserMessage(req, ctx) {
      const caller = requireAuth(ctx.values);
      const ok = await _forwardSimple(deps.db, req.sessionId, caller, {
        kind: "user-message" as const,
        session_id: asSessionId(req.sessionId),
        text: req.text,
      } as ClientControlFrame);
      return create(SessionsUserMessageResponseSchema, { accepted: ok });
    },

    async sessionsInput(req, ctx) {
      const caller = requireAuth(ctx.values);
      const cached = getCachedSessionWorker(req.sessionId);
      let row: { worker_fp: string; channel: number } | undefined = cached;
      if (!row) {
        const dbRow = await deps.db.selectFrom("sessions").select(["worker_fp", "channel"]).where("id", "=", req.sessionId).executeTakeFirst();
        if (dbRow) {
          row = { worker_fp: dbRow.worker_fp, channel: dbRow.channel };
          cacheSessionWorker(req.sessionId, row.worker_fp, row.channel);
        }
      }
      if (!row) {
        diag("bytes.up_relay", { sid: req.sessionId, len: req.data.length, ok: false, why: "no_session" });
        return create(SessionsInputResponseSchema, { accepted: false });
      }
      const sock = getWorkerHubSocket(row.worker_fp);
      if (!sock) {
        diag("bytes.up_relay", { sid: req.sessionId, len: req.data.length, ok: false, why: "no_worker_sock", worker_fp: row.worker_fp });
        return create(SessionsInputResponseSchema, { accepted: false });
      }
      const payload = req.data;
      const frame = new Uint8Array(3 + payload.length);
      new DataView(frame.buffer).setUint16(0, row.channel, false);
      frame[2] = 1; // DIR_TO_PTY
      frame.set(payload, 3);
      try {
        sock.send(frame);
        diag("bytes.up_relay", {
          sid: req.sessionId, len: payload.length, sha8: _coordSha8(payload),
          ok: true, worker_fp: row.worker_fp, channel: row.channel, caller_fp: caller.fingerprint,
        });
        return create(SessionsInputResponseSchema, { accepted: true });
      } catch (e) {
        log.warn("sessions.input.connect", "send_failed", { error: String(e) });
        diag("bytes.up_relay", { sid: req.sessionId, len: payload.length, ok: false, why: "send_throw", err: String(e) });
        return create(SessionsInputResponseSchema, { accepted: false });
      }
    },

    async sessionsCursorPos(req, ctx) {
      const caller = requireAuth(ctx.values);
      const row = await deps.db.selectFrom("sessions").select(["worker_fp", "channel"]).where("id", "=", req.sessionId).executeTakeFirst();
      if (row) {
        const { publishPresence } = await import("../presence-hub.ts");
        publishPresence(row.worker_fp, row.channel, {
          kind: "presence-delta",
          channel_id: row.channel,
          viewer_id: caller.fingerprint,
          cursor_col: req.col, cursor_row: req.row,
          label: caller.fingerprint.slice(0, 8),
        });
      }
      const ok = await _forwardSimple(deps.db, req.sessionId, caller, {
        kind: "cursor-pos" as const,
        session_id: asSessionId(req.sessionId),
        col: req.col, row: req.row,
      } as ClientControlFrame);
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


    async sessionsGetScrollbackCells(req, ctx) {
      const caller = requireAuth(ctx.values);
      const row = await deps.db.selectFrom("sessions").select(["worker_fp"]).where("id", "=", req.sessionId).executeTakeFirst();
      if (!row) throw new ConnectError("unknown session", Code.NotFound);
      const sock = getWorkerHubSocket(row.worker_fp);
      if (!sock) throw new ConnectError("worker not connected", Code.Unavailable);
      const pending = createPendingRpc<{ rows: CellRow[]; cols: number; total: number; start_row: number; end_row: number }>(8_000, row.worker_fp);
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "get-scrollback-cells" as const,
        request_id: pending.request_id,
        session_id: asSessionId(req.sessionId),
        end_row: Number(req.endRow),
        max_rows: req.maxRows,
      });
      let res;
      try { res = await pending.promise; }
      catch {
        // THROW on the 8s pending-rpc timeout: the SPA's backfill controller
        // retries/parks; an empty response would read as "no history".
        throw new ConnectError("scrollback cells serve timed out", Code.Unavailable);
      }
      return create(SessionsGetScrollbackCellsResponseSchema, {
        rows: res.rows.map(cellRowToProto),
        cols: res.cols,
        scrollbackTotal: BigInt(res.total),
        startRow: BigInt(res.start_row),
        endRow: BigInt(res.end_row),
      });
    },

    async sessionsGetChatHistory(req, ctx) {
      requireAuth(ctx.values);
      const row = await deps.db.selectFrom("sessions").select(["worker_fp"]).where("id", "=", req.sessionId).executeTakeFirst();
      if (!row) throw new ConnectError("unknown session", Code.NotFound);
      const sock = getWorkerHubSocket(row.worker_fp);
      if (!sock) throw new ConnectError("worker not connected", Code.Unavailable);
      const pending = createPendingRpc<{ messages: ChatMessage[]; next_seq: number; truncated: boolean }>(8_000, row.worker_fp);
      sendBrowserCmd(sock, requireAuth(ctx.values), pending.request_id, {
        kind: "get-chat-history" as const,
        request_id: pending.request_id,
        session_id: asSessionId(req.sessionId),
        ...(req.afterSeq !== undefined ? { after_seq: Number(req.afterSeq) } : {}),
        max_messages: req.maxMessages || 500,
      });
      let res;
      try { res = await pending.promise; }
      catch { throw new ConnectError("chat history serve timed out", Code.Unavailable); }
      return create(SessionsGetChatHistoryResponseSchema, {
        messages: res.messages.map(chatMessageToProto),
        nextSeq: BigInt(res.next_seq),
        truncated: res.truncated,
      });
    },

    async sessionsGetChatBlock(req, ctx) {
      requireAuth(ctx.values);
      const row = await deps.db.selectFrom("sessions").select(["worker_fp"]).where("id", "=", req.sessionId).executeTakeFirst();
      if (!row) throw new ConnectError("unknown session", Code.NotFound);
      const sock = getWorkerHubSocket(row.worker_fp);
      if (!sock) throw new ConnectError("worker not connected", Code.Unavailable);
      const pending = createPendingRpc<{ text: string }>(8_000, row.worker_fp);
      sendBrowserCmd(sock, requireAuth(ctx.values), pending.request_id, {
        kind: "get-chat-block" as const,
        request_id: pending.request_id,
        session_id: asSessionId(req.sessionId),
        message_id: req.messageId,
        block_index: req.blockIndex,
      });
      let res;
      try { res = await pending.promise; }
      catch { throw new ConnectError("chat block serve timed out", Code.Unavailable); }
      return create(SessionsGetChatBlockResponseSchema, { text: res.text });
    },

    async sessionsChatCommand(req, ctx) {
      requireAuth(ctx.values);
      const row = await deps.db.selectFrom("sessions").select(["worker_fp"]).where("id", "=", req.sessionId).executeTakeFirst();
      if (!row) throw new ConnectError("unknown session", Code.NotFound);
      const sock = getWorkerHubSocket(row.worker_fp);
      if (!sock) throw new ConnectError("worker not connected", Code.Unavailable);
      // 35s: prompt acks fast, but get_state/first-command lazy-boot takes seconds.
      const pending = createPendingRpc<{ response_json: string }>(35_000, row.worker_fp);
      sendBrowserCmd(sock, requireAuth(ctx.values), pending.request_id, {
        kind: "chat-command" as const,
        request_id: pending.request_id,
        session_id: asSessionId(req.sessionId),
        command_json: req.commandJson,
      });
      let res;
      // Distinguish the three failure modes the SPA used to see as one opaque
      // "Unavailable": a worker too old to know `chat-command` rejects it
      // immediately, and reporting that as a timeout sent debugging the wrong
      // way for an hour. The worker's own message is the useful part.
      try { res = await pending.promise; }
      catch (e) {
        const detail = e instanceof Error ? e.message : String(e);
        throw new ConnectError(`chat command failed on worker: ${detail}`, Code.Unavailable);
      }
      return create(SessionsChatCommandResponseSchema, { responseJson: res.response_json });
    },
  };
}
