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


export interface RootState {
  workers: Record<string, Worker>;           // key = WorkerFp
  sessions: Record<string, Session>;         // key = SessionId
  workspaces: Record<string, Workspace>;     // key = WorkspaceId
  tasks: Record<string, Task>;               // key = TaskId
  permission_rules: Record<string, PermissionRule>; // key = PermissionRuleId
  mcp_relays: Record<string, McpRelay>;      // key = McpRelayId
  pair_requests: Record<string, PairRequest>; // key = ephemeral_id
  coord_identity: { fingerprint_hex: string; git_sha: string; public_url: string; relocated_to_url?: string; handoff_id?: string } | null;
  /** OSC-0/OSC-2 title from the terminal core. Empty until the program sets
   *  one. */
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
  terminal_title: {},
  last_activity: {},
  session_viewers: {},
  browser_unauthorized: false,
};

export const [rootStore, setRootStore] = createStore<RootState>(initialState);
