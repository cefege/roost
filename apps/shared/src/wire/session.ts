// Session = atomic unit. Everything in the sidebar derives from this.
// REWRITE.md R3.

import { z } from "zod";
import { ChannelId, SessionId, WorkerFp, WorkspaceId } from "./brand.ts";

// ─── AgentState (claude only; null for plain shell) ─────────────────────

export const ClaudeMode = z.enum([
  "default",
  "acceptEdits",
  "plan",
  "bypassPermissions",
  "dontAsk",
  "auto",
]);
export type ClaudeMode = z.infer<typeof ClaudeMode>;

export const AgentStatus = z.enum(["running", "needs-input", "idle", "done"]);
export type AgentStatus = z.infer<typeof AgentStatus>;

export const MessageRole = z.enum(["user", "assistant", "thinking"]);
export type MessageRole = z.infer<typeof MessageRole>;

export const Tokens = z.object({
  in: z.number().int().nonnegative(),
  out: z.number().int().nonnegative(),
  cached: z.number().int().nonnegative(),
});
export type Tokens = z.infer<typeof Tokens>;

export const LastMessage = z.object({
  role: MessageRole,
  text: z.string(),
  ts: z.number().int().positive(),
});
export type LastMessage = z.infer<typeof LastMessage>;

export const CurrentTool = z.object({
  name: z.string().min(1),
  input_summary: z.string(),
});
export type CurrentTool = z.infer<typeof CurrentTool>;

export const CurrentBlock = z.object({
  id: z.number().int().nonnegative(),
  command: z.string().nullable(),
});
export type CurrentBlock = z.infer<typeof CurrentBlock>;

export const PermissionRequest = z.object({
  id: z.string().min(1),
  snippet: z.string(),
  options: z.array(z.string()).min(1),
});
export type PermissionRequest = z.infer<typeof PermissionRequest>;

export const SubAgentRow = z.object({
  parent_message_id: z.string().min(1),
  child_session_id: z.string().min(1),
  label: z.string(),
  status: AgentStatus,
});
export type SubAgentRow = z.infer<typeof SubAgentRow>;

export const AgentState = z.object({
  kind: z.literal("claude"),
  mode: ClaudeMode,
  model: z.string(),
  status: AgentStatus,
  tokens: Tokens,
  cost_usd: z.number().nonnegative(),
  last_message: LastMessage.nullable(),
  current_tool: CurrentTool.nullable(),
  current_block: CurrentBlock.nullable(),
  permission_request: PermissionRequest.nullable(),
  sub_agents: z.array(SubAgentRow),
  // A1: live state is stale (worker restarted; the bridge that produces
  // agent state can't be re-attached). Terminal still works; reopen to
  // refresh. Optional — absent/false on every live agent.
  stale: z.boolean().optional(),
});
export type AgentState = z.infer<typeof AgentState>;

// Default AgentState for first-agent-event-into-null-agent merges.
// foldEvent uses this so a partial patch like { status: "running" } produces
// a fully populated AgentState instead of an unsafe `as AgentState` cast.
// Worker emitters set what they know; defaults fill the rest.
export function defaultAgentState(): AgentState {
  return {
    kind: "claude",
    mode: "default",
    model: "",
    status: "running",
    tokens: { in: 0, out: 0, cached: 0 },
    cost_usd: 0,
    last_message: null,
    current_tool: null,
    current_block: null,
    permission_request: null,
    sub_agents: [],
  };
}

// ─── Session ────────────────────────────────────────────────────────────

export const SessionKind = z.enum(["shell", "claude"]);
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
  agent: AgentState.nullable(),             // null for plain shell
  created_at: z.number().int().positive(),
  closed_at: z.number().int().positive().nullable(),
  // User rename (sticky override of the auto title). null = no override.
  // Coord/DB-owned: the worker doesn't track it, so snapshot fold preserves
  // the prior value (like workspace_id). See sessionTitle.ts precedence.
  custom_title: z.string().nullable(),
  // Local git branch of cwd, resolved on the worker host. Optional +
  // additive (like AgentState.stale): absent = not resolved / not a repo.
  // Set by the `git` SessionEvent; feeds the cell-grid folder-row subtitle.
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
}).refine(
  // D-3 invariant: shell sessions cannot carry agent state. Claude
  // sessions MAY have agent:null briefly between spawn and the first
  // `agent` SessionEvent that seeds AgentState.
  (s) => s.kind !== "shell" || s.agent === null,
  { message: "shell session must have agent:null" },
);
export type Session = z.infer<typeof Session>;
