// Single Solid createStore-backed root. All state lives here.
// Components subscribe to selectors (store/selectors.ts), never mutate.
// Sync logic in store/sync.ts; event projection in store/projector.ts.
// R0.4-ONE-STORE-WEB.

import { batch } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import type {
  Worker,
  Session,
  Workspace,
  Task,
  McpRelay,
  AgentStatus,
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
  userAgent: string;
  clientBrowser: string;
  clientOs: string;
  clientDeviceType: string;
  sourceIp: string;
  countryCode: string;
  region: string;
  city: string;
  edgeIdentityProvider: string;
  edgeIdentity: string;
  edgeIdentityVerified: boolean;
  expiresAtMs: number;
}

export interface RootState {
  /** Advances whenever this browser's authenticated resources become obsolete. */
  auth_generation: number;
  workers: Record<string, Worker>;
  sessions: Record<string, Session>;
  workspaces: Record<string, Workspace>;
  tasks: Record<string, Task>;
  mcp_relays: Record<string, McpRelay>;
  pair_requests: Record<string, PairRequest>;
  agent_status: Record<string, AgentStatus>;
  coord_identity: {
    git_sha: string;
    public_url: string;
  } | null;
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
   *  refresh once authorization succeeds. See AllView.tsx + SidebarEmptyState. */
  browser_unauthorized: boolean;
}

const initialState: RootState = {
  auth_generation: 0,
  workers: {},
  sessions: {},
  workspaces: {},
  tasks: {},
  mcp_relays: {},
  pair_requests: {},
  agent_status: {},
  coord_identity: null,
  terminal_title: {},
  last_activity: {},
  session_viewers: {},
  browser_unauthorized: false,
};

export const [rootStore, setRootStore] = createStore<RootState>(initialState);

/** Invalidate every token held by credential-bound asynchronous work before
 * the corresponding server request can settle. */
export function invalidateAuthResources(): void {
  setRootStore("auth_generation", (generation) => generation + 1);
}

/** Clear every record populated by an authenticated list/snapshot. Coordinator
 * identity deliberately survives so the pairing surfaces can still render. */
export function clearAuthScopedRootData(): void {
  setRootStore("workers", reconcile({}));
  setRootStore("sessions", reconcile({}));
  setRootStore("workspaces", reconcile({}));
  setRootStore("tasks", reconcile({}));
  setRootStore("mcp_relays", reconcile({}));
  setRootStore("pair_requests", reconcile({}));
  setRootStore("agent_status", reconcile({}));
  setRootStore("terminal_title", reconcile({}));
  setRootStore("last_activity", reconcile({}));
  setRootStore("session_viewers", reconcile({}));
}

/** Remove every authenticated replica and invalidate outstanding tokens.
 * Coordinator discovery survives so the pairing surfaces can still render. */
export function clearAccountRootStateForLogout(): void {
  batch(() => {
    clearAuthScopedRootData();
    invalidateAuthResources();
    setRootStore("browser_unauthorized", false);
  });
}

// Slices keyed as plain-object Records. Solid setStore cannot delete through
// a setter function on such a subtree (feedback_solid_setstore_record_replace)
// — the removal primitive IS the undefined write, so the one deliberate
// value-slot cast lives HERE instead of repeated at every mutation site.
type RecordSliceKey = {
  [K in keyof RootState]: RootState[K] extends Record<string, unknown> ? K : never;
}[keyof RootState];
/** Remove one key from a root-store Record slice (per-key delete).
 *  Solid's deep-setter types cannot express a generic slice+key pair, so
 *  the single structural cast lives here — callers stay typed. */
export function deleteStoreRecord<K extends RecordSliceKey>(slice: K, key: string): void {
  type RawSetter = (slice: string, key: string, value: unknown) => void;
  (setRootStore as unknown as RawSetter)(slice, key, undefined);
}
