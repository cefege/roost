// SessionEvent = append-only log row. Coord's `events` table is the
// source of truth; Session projections = fold(events). Browser folds
// the same events from the SSE stream. R0.3, R3.1.

import { z } from "zod";
import { Session, SessionKind } from "./session.ts";
import { ChannelId, SessionId, WorkerFp, WorkspaceId, TraceId } from "./brand.ts";

const Base = z.object({
  ts: z.number().int().positive(),
  trace_id: TraceId.optional(),
});

export const SessionEvent = z.discriminatedUnion("kind", [
  Base.extend({
    kind: z.literal("opened"),
    session_id: SessionId,
    worker_fp: WorkerFp,
    channel: ChannelId,
    session_kind: SessionKind,
    cwd: z.string(),
  }),
  Base.extend({
    kind: z.literal("closed"),
    session_id: SessionId,
    exit_code: z.number().int().nullable(),
  }),
  Base.extend({
    kind: z.literal("attached"),
    session_id: SessionId,
  }),
  Base.extend({
    kind: z.literal("detached"),
    session_id: SessionId,
  }),
  Base.extend({
    kind: z.literal("cwd"),
    session_id: SessionId,
    cwd: z.string(),
  }),
  Base.extend({
    kind: z.literal("workspace_assigned"),
    session_id: SessionId,
    workspace_id: WorkspaceId.nullable(),
  }),
  Base.extend({
    // Worker re-announces all live sessions on coord reconnect.
    // Coord reconciles: any DB session for this worker NOT in the
    // snapshot gets a synthetic `closed` event appended.
    kind: z.literal("snapshot"),
    worker_fp: WorkerFp,
    sessions: z.array(Session),
  }),
  Base.extend({
    // Worker rebooted; the keeper PTY for this session died. Worker
    // spawned a fresh PTY at the same cwd and re-bound it to the same
    // session_id. Sidebar row stays in place.
    kind: z.literal("respawned"),
    session_id: SessionId,
    new_channel: ChannelId,
  }),
  Base.extend({
    // User renamed the session from the sidebar. custom_title="" clears the
    // override (revert to auto title). Sticky: auto-title events never touch
    // custom_title, so a rename survives OSC-title churn.
    kind: z.literal("renamed"),
    session_id: SessionId,
    custom_title: z.string(),
  }),
  Base.extend({
    // Worker resolved / re-resolved the git branch of the session's cwd.
    // branch=null → folder isn't a git repo. Feeds the cell-grid folder-row
    // subtitle (FolderList.tsx). Emitted on spawn + on .git/HEAD change.
    kind: z.literal("git"),
    session_id: SessionId,
    branch: z.string().nullable(),
    // GitHub owner/repo — present only when resolved; absent = leave alone.
    remote: z.string().optional(),
  }),
  Base.extend({
    // Worker-resolved GitHub PR status for the session's branch (via
    // `gh pr list --head <branch>`). number=null → no open PR for the branch.
    // Feeds the #123 ✓ folder-row badge (FolderList.tsx). Emitted on
    // spawn + branch-change + a 90s poll, deduped to changes only.
    kind: z.literal("pr"),
    session_id: SessionId,
    number: z.number().int().nullable(),
    state: z.enum(["open", "merged", "closed", "draft"]).nullable(),
    checks: z.enum(["passing", "failing", "pending", "none"]).nullable(),
    url: z.string().nullable(),
  }),
  Base.extend({
    // Worker-detected LISTEN ports of the session's process tree (ps+lsof).
    // Feeds the :5174 folder-row chips (FolderList.tsx). Emitted on spawn +
    // a 90s poll, deduped to changes. Empty = nothing listening.
    kind: z.literal("ports"),
    session_id: SessionId,
    ports: z.array(z.number().int()),
  }),
]);
export type SessionEvent = z.infer<typeof SessionEvent>;

// Fold one event into a session map. Pure function — replay is
// deterministic so long as the input order is stable.
export function foldEvent(
  prev: Map<string, Session>,
  e: SessionEvent,
): Map<string, Session> {
  const next = new Map(prev);
  switch (e.kind) {
    case "opened":
      next.set(e.session_id, {
        id: e.session_id,
        worker_fp: e.worker_fp,
        channel: e.channel,
        kind: e.session_kind,
        cwd: e.cwd,
        // Immutable spawn folder — captured once here from the opened cwd. The
        // `cwd` event below updates `cwd` only, never this. Backs the /t/ URL.
        spawn_cwd: e.cwd,
        workspace_id: null,
        status: "open",
        created_at: e.ts,
        closed_at: null,
        custom_title: null,
      });
      return next;
    case "closed": {
      // The terminal's process actually exited (worker emits `closed` only on
      // real PTY exit) or coord confirmed it's gone from the worker's
      // authoritative live snapshot (ghost). DELETE the row — no "closed"
      // limbo. A LIVE terminal is always in the worker snapshot, so it is
      // never seen as closed and never reaches this branch. `closed` is the
      // ONLY deletion trigger.
      if (!prev.has(e.session_id)) return prev;
      next.delete(e.session_id);
      return next;
    }
    case "attached":
    case "detached":
      return prev; // pure liveness; no projection change yet
    case "cwd": {
      const s = prev.get(e.session_id);
      if (!s) return prev;
      next.set(e.session_id, { ...s, cwd: e.cwd });
      return next;
    }
    case "workspace_assigned": {
      const s = prev.get(e.session_id);
      if (!s) return prev;
      next.set(e.session_id, { ...s, workspace_id: e.workspace_id });
      return next;
    }
    case "snapshot": {
      // Sync this worker's ANNOUNCED sessions into the projection (upsert). A
      // session of this worker that is ABSENT from the snapshot is NOT deleted —
      // it persists as an offline "breadcrumb" row: a worker restart kills the
      // PTY but the row survives so the sidebar still shows where you were
      // working. A reconnect respawn re-binds it; an explicit `closed` (real PTY
      // exit) or the user's ✕ removes it. Other workers' sessions are untouched.
      // (This mirrors coord's SQL snapshot reconcile in event-log.ts — both stop
      // pruning ghosts, keeping the SPA + coord projections in agreement.)
      for (const s of e.sessions) {
        const before = prev.get(s.id);
        // These fields are coordinator/DB-owned or immutable creation facts.
        // A reconnect snapshot refreshes the live worker state without
        // rewriting history or user organization.
        next.set(s.id, before
          ? {
              ...s,
              created_at: before.created_at,
              workspace_id: before.workspace_id,
              custom_title: before.custom_title,
              spawn_cwd: before.spawn_cwd,
            }
          : s);
      }
      return next;
    }
    case "respawned": {
      const s = prev.get(e.session_id);
      if (!s) return prev;
      // New keeper channel. Status is forced open in case a prior 'closed'
      // was projected before the respawn lands.
      next.set(e.session_id, {
        ...s,
        channel: e.new_channel,
        status: "open",
        closed_at: null,
      });
      return next;
    }
    case "renamed": {
      const s = prev.get(e.session_id);
      if (!s) return prev;
      // "" clears the override → revert to auto title. Auto-title events
      // (OSC/cwd) never write custom_title, so this is sticky.
      next.set(e.session_id, { ...s, custom_title: e.custom_title || null });
      return next;
    }
    case "git": {
      const s = prev.get(e.session_id);
      if (!s) return prev;
      // branch always present; remote only when the worker resolved a github
      // origin (absent → preserve the prior git_remote).
      next.set(e.session_id, e.remote !== undefined
        ? { ...s, git_branch: e.branch, git_remote: e.remote }
        : { ...s, git_branch: e.branch });
      return next;
    }
    case "pr": {
      const s = prev.get(e.session_id);
      if (!s) return prev;
      next.set(e.session_id, {
        ...s,
        pr_number: e.number,
        pr_state: e.state,
        pr_checks: e.checks,
        pr_url: e.url,
      });
      return next;
    }
    case "ports": {
      const s = prev.get(e.session_id);
      if (!s) return prev;
      next.set(e.session_id, { ...s, ports: e.ports });
      return next;
    }
  }
}

export function foldAll(events: SessionEvent[]): Map<string, Session> {
  let acc = new Map<string, Session>();
  for (const e of events) acc = foldEvent(acc, e);
  return acc;
}
