// Type surface of the page's SPA phase timeline: the bounded list of
// navigation-relative phase marks `window.__smoke.phaseTimeline()` returns.
// Type-only; read by the perf probes.

export type SpaPhaseName = "module_start" | "identity_complete" | "sync_subscribed" | "snapshot_complete" | "snapshot_applied" | "sessions_list_publish" | "terminal_mount" | "viewport_enqueue" | "viewport_accept" | "first_cell_receive" | "first_cell_apply" | "marker_presented" | "cursor_presented";

export type SpaPhaseMark = {
  index: number;
  name: SpaPhaseName;
  monotonicMs: number;
  epochMs: number;
  sinceNavigationMs: number;
  onceKey?: string | undefined;
  detail: Record<string, string | number | boolean | null>;
};

export type SpaPhaseTimeline = {
  capacity: number;
  dropped: number;
  timeOriginEpochMs: number;
  navigationStartEpochMs: number;
  driverBeforeNavigationEpochMs: number | null;
  marks: SpaPhaseMark[];
};
