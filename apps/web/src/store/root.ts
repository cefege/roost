// Single Solid createStore-backed root. All state lives here.
// Components subscribe to selectors (store/selectors.ts), never mutate.
// Sync logic in store/sync.ts; event projection in store/projector.ts.
// R0.4-ONE-STORE-WEB.

import { createStore } from "solid-js/store";
import type {
  Worker,
  Session,
  Workspace,
  Task,
  PermissionRule,
  McpRelay,
} from "@roost/shared/wire";
import type { ChatMessage } from "@roost/shared/chat/wire";

// Keyed by string id for plain-object Solid reactivity.
// Solid createStore + Map<K,V> has limited granularity; keyed records work better.
// Pending tap-to-pair requests. Shape mirrors pair.list output in
// apps/shared/src/router.ts (no dedicated wire type — pair domain has
// only inline output shapes). key = ephemeral_id.
export interface PairRequest {
  ephemeral_id: string;
  label: string;
  created_at_ms: number;
}

/**
 * claude_status — a per-session MIRROR of the session's `agent.status` (the
 * worker arbiter's single authority), written by the projector when an agent
 * patch folds. It exists so the few direct readers (TabBar dot, search-result
 * rows) keep a flat lookup without reaching into sessions[id].agent. The old
 * coord grid-scrape (claude-status-hub / @roost/shared/claude-status) was deleted
 * in the detection consolidation — there is now ONE detection algorithm.
 * `ClaudeStatus` is kept as an alias of the wire `AgentStatus` so existing
 * importers resolve unchanged.
 */
export type ClaudeStatus = "running" | "running-workflow" | "needs-input" | "idle" | "unknown";

export interface RootState {
  workers: Record<string, Worker>;           // key = WorkerFp
  sessions: Record<string, Session>;         // key = SessionId
  workspaces: Record<string, Workspace>;     // key = WorkspaceId
  tasks: Record<string, Task>;               // key = TaskId
  permission_rules: Record<string, PermissionRule>; // key = PermissionRuleId
  mcp_relays: Record<string, McpRelay>;      // key = McpRelayId
  pair_requests: Record<string, PairRequest>; // key = ephemeral_id
  coord_identity: { fingerprint_hex: string; git_sha: string } | null;
  claude_status: Record<string, ClaudeStatus>; // key = SessionId
  /** OSC-0/OSC-2 title from the wterm core. Empty when the shell/claude
   *  hasn't set one yet. Written by Terminal.tsx's onTitle callback;
   *  consumed by sidebar/SessionRow.tsx as the preferred row label
   *  (a display-priority pattern). */
  terminal_title: Record<string, string>;
  /** Coord-stamped last-activity timestamp (ms) per session, from PTY byte
   *  flow (last-activity-hub, throttled). Used by the sidebar "Last activity"
   *  filter to age out idle OPEN sessions. Missing = no activity seen since
   *  coord started → consumer falls back to created_at. key = SessionId */
  last_activity: Record<string, number>;
  /** Per-session list of viewers currently looking at the session.
   *  Each entry carries the browser's full fingerprint + its
   *  container-measured (cols, rows) + lastMs (focus/input timestamp).
   *  Terminal sizes its wterm to the entry with max(lastMs) — the
   *  latest-window-size policy. SessionRow renders one dot per fp.
   *  lastMs may be missing on legacy/older coord builds; consumers
   *  treat undefined as 0 (deterministic loser). */
  session_viewers: Record<string, Array<{ fp: string; cols: number; rows: number; lastMs?: number; label?: string; viewerKey?: string }>>;
  /** True when bootstrap saw a Connect `unauthenticated` code on the
   *  authed list calls (workersList / sessionsList / workspacesList).
   *  Drives the sidebar's `browser-unpaired` empty-state kind +
   *  routes the CTA to /pair (Onboarding). Cleared on successful
   *  refresh once trust is granted. See AllView.tsx + SidebarEmptyState. */
  browser_unauthorized: boolean;
  /** Omp chat. Per-session messages + status for `kind:"agent"` sessions.
   *  Self-contained omp slice — no shared chat components. The logic +
   *  selectors live in store/chatOmp.ts; state mounts here so sync.ts's
   *  single reactive flush covers chat frames too. key = SessionId. */
  chat_omp: Record<string, ChatOmpState>;
}

/** `failed` = the backfill RPC errored: the pane has NO history and cannot get
 *  it. Distinct from `resolved` with zero messages, which is a genuinely empty
 *  conversation — conflating the two painted a welcome card over a dead pipe. */
export type ChatOmpStatus = "idle" | "loading" | "resolved" | "failed";
export interface ChatOmpState {
  messages: ChatMessage[];
  seq: number;
  status: ChatOmpStatus;
  /** An agent turn is in flight (worker-owned flag). */
  streaming: boolean;
  /** Session status, from the RPC child's get_state. Empty/0 = the fact is
   *  unknown, and its chip is not rendered. */
  model: string;
  modelName: string;
  thinkingLevel: string;
  contextTokens: number;
  /** Model context window. 0 = unknown (no catalog entry) → the pane shows
   *  raw tokens with no percentage, exactly as omp's own bar does. */
  contextWindow: number;
  /** omp agent mode ("plan", "none", …). omp's RPC get_state exposes no mode,
   *  so this is "" today; kept because the pane renders it when present. */
  mode: string;
}

const initialState: RootState = {
  workers: {},
  sessions: {},
  workspaces: {},
  tasks: {},
  permission_rules: {},
  mcp_relays: {},
  pair_requests: {},
  coord_identity: null,
  claude_status: {},
  terminal_title: {},
  last_activity: {},
  session_viewers: {},
  browser_unauthorized: false,
  chat_omp: {},
};

export const [rootStore, setRootStore] = createStore<RootState>(initialState);
