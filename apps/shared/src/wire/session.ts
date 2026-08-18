// Session = atomic unit. Everything in the sidebar derives from this.

import { z } from "zod";
import { ChannelId, SessionId, WorkerFp, WorkspaceId } from "./brand.ts";


// ─── Session ────────────────────────────────────────────────────────────

// Every Roost session is a keeper-backed shell terminal.
export const SessionKind = z.enum(["shell"]);
export type SessionKind = z.infer<typeof SessionKind>;

export const SessionStatus = z.enum(["open", "closed"]);
export type SessionStatus = z.infer<typeof SessionStatus>;

export const Session = z.object({
  id: SessionId,
  worker_fp: WorkerFp,
  channel: ChannelId,
  kind: SessionKind,
  cwd: z.string(),
  // The folder the session was SPAWNED in — set once on `opened`, NEVER updated
  // by `cwd` events (unlike `cwd`, which drifts as the shell cd's). This is the
  // stable identity behind the /t/:workerFp/*folderPath URL. Additive/optional:
  // absent/null on pre-migration rows → callers fall back to /s/:sessionId.
  spawn_cwd: z.string().nullable().optional(),
  workspace_id: WorkspaceId.nullable(),     // null = orphan → Inbox bucket
  status: SessionStatus,
  created_at: z.number().int().positive(),
  closed_at: z.number().int().positive().nullable(),
  // User rename (sticky override of the auto title). null = no override.
  // Coord/DB-owned: the worker doesn't track it, so snapshot fold preserves
  // the prior value (like workspace_id). See sessionTitle.ts precedence.
  custom_title: z.string().nullable(),
  // Local git branch of cwd, resolved on the worker host. Optional + additive:
  // absent = not resolved / not a repo. Set by the `git` SessionEvent.
  git_branch: z.string().nullable().optional(),
  // GitHub "owner/repo" of the session's origin remote (github.com only).
  // Additive/optional; feeds bare #123 / commit-SHA terminal links.
  git_remote: z.string().nullable().optional(),
  // GitHub PR status for git_branch, resolved on the worker via `gh pr list`.
  // All additive/optional. pr_number null/absent = no open PR. Set by the
  // `pr` SessionEvent; feeds the #123 ✓ folder-row badge (FolderList.tsx).
  pr_number: z.number().int().nullable().optional(),
  pr_state: z.enum(["open", "merged", "closed", "draft"]).nullable().optional(),
  pr_checks: z.enum(["passing", "failing", "pending", "none"]).nullable().optional(),
  pr_url: z.string().nullable().optional(),
  // TCP ports the session's process tree is LISTENing on (worker lsof).
  // Additive/optional; feeds :5174 folder-row chips. Set by `ports` event.
  ports: z.array(z.number().int()).optional(),
});
export type Session = z.infer<typeof Session>;
