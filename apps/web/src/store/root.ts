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
}
/** Server-returned account and membership scope. These are presentation data;
 * the coordinator remains the only authorization authority. */
export interface OrganizationScope {
  id: string;
  slug: string;
  name: string;
  role: string;
}

export interface DashboardScope {
  id: string;
  organization_id: string;
  slug: string;
  name: string;
  organization_role: string;
  dashboard_role: string;
}

export interface DashboardAccessSnapshot {
  account_id: string;
  organizations: readonly OrganizationScope[];
  dashboards: readonly DashboardScope[];
  selected_dashboard_id: string | null;
  capabilities: readonly string[];
}



export interface RootState {
  /** Server-confirmed account for the current browser device. */
  account_id: string | null;
  /** Active organization memberships returned by AuthDashboardAccess. */
  organizations: Record<string, OrganizationScope>;
  /** Active dashboard memberships returned by AuthDashboardAccess. */
  dashboards: Record<string, DashboardScope>;
  /** The only dashboard sent on tenant transport requests. */
  selected_dashboard_id: string | null;
  /** Opaque effective capability names from the coordinator. */
  effective_capabilities: string[];
  /** Advances whenever the current dashboard's resources become obsolete. */
  dashboard_generation: number;
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
    public_listener: boolean;
    saas_mode: boolean;
    relocated_to_url?: string;
    handoff_id?: string;
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
  account_id: null,
  organizations: {},
  dashboards: {},
  selected_dashboard_id: null,
  effective_capabilities: [],
  dashboard_generation: 0,
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

function accessRecords<T extends { id: string }>(values: readonly T[]): Record<string, T> {
  return Object.fromEntries(values.map((value) => [value.id, { ...value }]));
}

export function isValidDashboardAccess(snapshot: DashboardAccessSnapshot): boolean {
  const dashboards = accessRecords(snapshot.dashboards);
  const organizations = accessRecords(snapshot.organizations);
  const selected = snapshot.selected_dashboard_id;
  if (selected === null) return Object.keys(dashboards).length === 0;
  const dashboard = dashboards[selected];
  return !!dashboard && !!organizations[dashboard.organization_id];
}

/** Replace only server-confirmed scope metadata. A malformed response must not
 * turn a browser hint into an active dashboard. */
export function setDashboardAccess(snapshot: DashboardAccessSnapshot): boolean {
  if (!isValidDashboardAccess(snapshot)) return false;
  const selected = snapshot.selected_dashboard_id;
  batch(() => {
    const selectionChanged = rootStore.selected_dashboard_id !== selected;
    setRootStore("account_id", snapshot.account_id || null);
    setRootStore("organizations", reconcile(accessRecords(snapshot.organizations)));
    setRootStore("dashboards", reconcile(accessRecords(snapshot.dashboards)));
    setRootStore("selected_dashboard_id", selected);
    setRootStore("effective_capabilities", reconcile([...snapshot.capabilities]));
    if (selectionChanged) {
      setRootStore("dashboard_generation", (generation) => generation + 1);
    }
  });
  return true;
}

/** Invalidate every token held by dashboard-bound asynchronous work before
 * the corresponding server request can settle. */
export function invalidateDashboardResources(): void {
  setRootStore("dashboard_generation", (generation) => generation + 1);
}

/** Clear every record populated by a dashboard-scoped list/snapshot. Scope
 * metadata and coordinator identity deliberately survive until the next
 * server-confirmed access response replaces them. */
export function clearDashboardScopedRootData(): void {
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

/** Remove account identity, memberships, selection, and all scoped replicas.
 * Coordinator discovery survives so the managed login route can still render. */
export function clearAccountRootStateForLogout(): void {
  batch(() => {
    clearDashboardScopedRootData();
    setRootStore("account_id", null);
    setRootStore("organizations", reconcile({}));
    setRootStore("dashboards", reconcile({}));
    setRootStore("selected_dashboard_id", null);
    setRootStore("effective_capabilities", reconcile([]));
    setRootStore("dashboard_generation", (generation) => generation + 1);
    setRootStore("browser_unauthorized", false);
  });
}

/** Current selected scope as a string suitable for transport propagation. */
export function selectedDashboardId(): string | null {
  return rootStore.selected_dashboard_id;
}

/** True only after AuthDashboardAccess returned an account, selected dashboard,
 * and membership metadata for that exact selection. */
export function hasConfirmedDashboardAccess(): boolean {
  const selected = rootStore.selected_dashboard_id;
  return !!rootStore.account_id && !!selected && !!rootStore.dashboards[selected];
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
