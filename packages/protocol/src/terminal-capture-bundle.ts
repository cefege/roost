// JSON shape of one terminal incident bundle — the owner-only artifact a
// worker writes as <capture-id>.json.gz. Written by the worker's incident
// storage, produced from the three layer recorders, and read back only by
// scripts/replay-terminal-incident.ts.
// Types only: limits live in terminal-capture.ts, validation in
// terminal-capture-validate.ts, comparison in terminal-capture-view.ts.
// uint64 stream offsets and sequences are decimal STRINGS: JSON numbers lose
// exact byte offsets past 2^53 and a byte offset is the parse boundary.

import type { CellGridFrame, CellRow } from "./cell/types.ts";
import type { TerminalGeometry } from "./viewport.ts";
import type { TerminalCanonicalDifference } from "./terminal-capture-view.ts";

export const TERMINAL_INCIDENT_SCHEMA = "roost.terminal-incident.v1";

export type TerminalCaptureLayer = "browser" | "coordinator" | "worker";

export type TerminalCaptureReason =
  | "manual"
  | "history_identity"
  | "viewport_model"
  | "worker_emission"
  | "pre_repair";

export type TerminalCaptureCoverage = "complete" | "partial" | "unavailable";

/** Machine-readable reason a replay boundary is missing. Never free text: the
 *  replay report branches on these, and a parser message could carry terminal
 *  content into a console. */
export type TerminalCoverageReason =
  | "complete"
  | "layer_unavailable"
  | "missing_initial_prefix"
  | "raw_prefix_evicted"
  | "missing_resize_boundary"
  | "core_export_unavailable"
  | "grid_budget_exceeded"
  | "sample_budget_exceeded"
  | "baseline_invalidated"
  | "segment_evicted"
  | "frame_over_budget"
  | "evidence_trimmed"
  | "capture_expired";

export interface TerminalCaptureCoverageReport {
  readonly cell_replay: TerminalCaptureCoverage;
  readonly cell_replay_reasons: readonly TerminalCoverageReason[];
  readonly core_replay: TerminalCaptureCoverage;
  readonly core_replay_reasons: readonly TerminalCoverageReason[];
  /** Sampled-checkpoint equality proves only its own checkpoints. */
  readonly core_comparison: TerminalCaptureCoverage;
  readonly core_comparison_reasons: readonly TerminalCoverageReason[];
}

/** Identity of the process that produced one layer's evidence. */
export interface TerminalCaptureProcessIdentity {
  readonly layer: TerminalCaptureLayer;
  /** Per-process UUID, minted at module load; distinguishes a restart. */
  readonly process_id: string;
  readonly git_sha: string;
  readonly artifact_version: string;
  /** Worker only: pinned terminal-core WASM identity. */
  readonly wasm_identity: string | null;
  readonly worker_fp: string | null;
  /** Browser only. */
  readonly viewer_id: string | null;
  readonly user_agent: string | null;
}

export interface TerminalCaptureStreamIdentity {
  readonly stream_id: string;
  readonly grid_epoch: string;
  /** Decimal uint64. */
  readonly seq: string;
  readonly base_seq: string | null;
  readonly cols: number;
  readonly rows: number;
}

export interface TerminalCaptureDropCounters {
  readonly records: number;
  readonly bytes: number;
  readonly rows: number;
  readonly raw_bytes: number;
  readonly samples: number;
}

/** One thing this bundle does NOT contain, named exactly. A capture that
 *  trimmed evidence to fit a budget is partial, never "complete". */
export interface TerminalCaptureOmission {
  readonly kind: "section" | "records" | "rows" | "raw" | "sample";
  readonly name: string;
  readonly reason: TerminalCoverageReason;
  readonly dropped_count: number;
  readonly dropped_bytes: number;
  /** Absolute decimal range when the omission is positional. */
  readonly range: { readonly start: string; readonly end: string } | null;
}

export interface TerminalCaptureLayerHeader {
  readonly layer: TerminalCaptureLayer;
  readonly captured_at_ms: number;
  readonly process: TerminalCaptureProcessIdentity;
  readonly stream: TerminalCaptureStreamIdentity | null;
  readonly geometry: TerminalGeometry | null;
  readonly dropped: TerminalCaptureDropCounters;
  readonly omissions: readonly TerminalCaptureOmission[];
}

export interface TerminalCaptureHistoryRange {
  readonly start: string;
  readonly end: string;
  readonly status: "present" | "evicted" | "unavailable";
  readonly rows: number;
}

// ─── worker ────────────────────────────────────────────────────────────────

/** One worker stream generation × core incarnation. A resize that changes the
 *  epoch opens a new segment; the preceding one is retained until ordinary
 *  bounded eviction so a resize cannot erase the evidence of its own defect. */
export interface TerminalWorkerSegment {
  readonly segment_id: string;
  readonly stream_id: string;
  readonly grid_epoch: string;
  readonly core_incarnation: number;
  readonly opened_at_ms: number;
  readonly closed_at_ms: number | null;
  readonly open_reason: "armed" | "stream_change" | "epoch_change" | "core_rebuild";
  readonly geometry: TerminalGeometry;
  /** Absolute raw byte offset at segment open, decimal. */
  readonly open_offset: string;
}

export type TerminalWorkerComparison =
  | "equal"
  | "different"
  | "unsampled"
  | "budget_skipped"
  | "baseline_invalid";

export interface TerminalWorkerEmissionRecord {
  readonly segment_id: string;
  readonly emitted_at_ms: number;
  readonly stream: TerminalCaptureStreamIdentity;
  readonly full: boolean;
  /** The exact accepted full or delta, as emitted. */
  readonly frame: CellGridFrame;
  readonly comparison: TerminalWorkerComparison;
  readonly difference: TerminalCanonicalDifference | null;
}

/** A fresh viewport-only core scan taken at one emission's exact generation and
 *  sequence, plus the emitted-frame fold it was compared against. */
export interface TerminalWorkerCoreSampleRecord {
  readonly segment_id: string;
  readonly sampled_at_ms: number;
  readonly stream: TerminalCaptureStreamIdentity;
  readonly elapsed_us: number;
  readonly core_frame: CellGridFrame;
  readonly fold_frame: CellGridFrame;
  readonly comparison: TerminalWorkerComparison;
  readonly difference: TerminalCanonicalDifference | null;
}

export interface TerminalWorkerSamplingStats {
  readonly sampled: number;
  readonly skipped_interval: number;
  readonly skipped_budget: number;
  readonly skipped_grid: number;
  readonly suppressed_until_ms: number | null;
  readonly max_elapsed_us: number;
}

export type TerminalWorkerResizeOutcome =
  | "accepted"
  | "rejected"
  | "lost_ack"
  | "recovered"
  | "core_failed"
  | "unknown";

export interface TerminalWorkerResizeRecord {
  readonly segment_id: string;
  readonly at_ms: number;
  readonly resize_seq: number;
  /** Absolute raw offset when the capture gate was installed. */
  readonly install_offset: string;
  /** Absolute raw offset of the keeper-acknowledged parse boundary; null when
   *  the boundary was never proven (lost ACK / rejected). Never a request time. */
  readonly boundary_offset: string | null;
  readonly from: TerminalGeometry;
  readonly to: TerminalGeometry;
  readonly outcome: TerminalWorkerResizeOutcome;
  readonly grid_epoch_before: string;
  readonly grid_epoch_after: string | null;
  readonly captured_bytes: number;
}

export interface TerminalWorkerRawRecord {
  readonly segment_id: string;
  readonly at_ms: number;
  /** Absolute raw byte offsets, decimal, end exclusive. */
  readonly start_offset: string;
  readonly end_offset: string;
  readonly base64: string;
}

/** Legacy raw-only tail from the always-on byte ring. Present for an unarmed or
 *  manual capture, where no recorder retained exact per-chunk offsets. */
export interface TerminalWorkerByteCaptureTail {
  readonly end_offset: string;
  readonly start_offset: string;
  readonly byte_length: number;
  readonly base64: string;
}

export interface TerminalWorkerSection extends TerminalCaptureLayerHeader {
  readonly layer: "worker";
  readonly segments: readonly TerminalWorkerSegment[];
  readonly emissions: readonly TerminalWorkerEmissionRecord[];
  readonly core_samples: readonly TerminalWorkerCoreSampleRecord[];
  readonly sampling: TerminalWorkerSamplingStats;
  readonly resizes: readonly TerminalWorkerResizeRecord[];
  readonly raw: readonly TerminalWorkerRawRecord[];
  readonly byte_capture: TerminalWorkerByteCaptureTail | null;
  readonly core_scrollback_tail: readonly CellRow[];
  readonly history_rows: readonly CellRow[];
  readonly history_ranges: readonly TerminalCaptureHistoryRange[];
  readonly scrollback_total: number;
  readonly scrollback_origin: string;
}

// ─── coordinator ───────────────────────────────────────────────────────────

export interface TerminalCoordinatorRecord {
  readonly at_ms: number;
  readonly stream: TerminalCaptureStreamIdentity;
  readonly admitted_full: boolean;
  readonly accepted: boolean;
  /** Canonical viewport AFTER admission, from the hub's own snapshot. */
  readonly canonical: CellGridFrame | null;
  readonly snapshot_state: "none" | "installing" | "installed" | "invalid";
  readonly send_state: "sent" | "queued" | "dropped" | "not_sent";
  readonly gap: { readonly from: string; readonly to: string } | null;
  readonly repair: "none" | "requested_full" | "invalidated" | "evicted";
}

export interface TerminalCoordinatorSection extends TerminalCaptureLayerHeader {
  readonly layer: "coordinator";
  readonly records: readonly TerminalCoordinatorRecord[];
  readonly snapshot: TerminalCaptureStreamIdentity | null;
  readonly valid: boolean;
}

// ─── browser ───────────────────────────────────────────────────────────────

export type TerminalBrowserPhase =
  | "pre_apply"
  | "pre_destructive"
  | "pre_history_insert"
  | "post_reconcile"
  | "current";

export type TerminalBrowserApplyMode = "full" | "delta" | "fallback_full";

export interface TerminalDomRow {
  /** DOM order within its container, 0-based. */
  readonly order: number;
  /** Absolute history index read off the node, or null for a viewport row. */
  readonly index: number | null;
  readonly columns: number;
  readonly fingerprint: number;
  readonly text: string;
  readonly span_count: number;
}

export interface TerminalBrowserPaintedState {
  readonly at_ms: number;
  readonly phase: TerminalBrowserPhase;
  readonly apply_mode: TerminalBrowserApplyMode | null;
  /** Replica canonical viewport, derived independently of the renderer. */
  readonly canonical: CellGridFrame | null;
  /** Renderer's COMMITTED painted watermark — never the incoming frame. */
  readonly committed: TerminalCaptureStreamIdentity | null;
  readonly pending: TerminalCaptureStreamIdentity | null;
  readonly painted_model_history: readonly CellRow[];
  readonly dom_history: readonly TerminalDomRow[];
  readonly dom_viewport: readonly TerminalDomRow[];
  readonly gaps: readonly TerminalCaptureHistoryRange[];
  readonly cursor: {
    readonly row: number;
    readonly col: number;
    readonly visible: boolean;
  } | null;
  readonly scroll: {
    readonly top: number;
    readonly height: number;
    readonly client_height: number;
    readonly row_height: number;
  } | null;
  readonly reader: {
    readonly intent: string;
    readonly reason: string | null;
    readonly hold_mask: number;
  } | null;
  readonly active: boolean;
  readonly visible: boolean;
  readonly omissions: readonly TerminalCaptureOmission[];
}

export interface TerminalBrowserEvent {
  readonly at_ms: number;
  readonly kind:
    | "frame_received"
    | "replica_admitted"
    | "replica_repair"
    | "history_page"
    | "render_scheduled"
    | "render_applied"
    | "render_failed"
    | "destructive_full";
  readonly stream: TerminalCaptureStreamIdentity | null;
  readonly apply_mode: TerminalBrowserApplyMode | null;
  readonly detail: string | null;
}

export interface TerminalBrowserSection extends TerminalCaptureLayerHeader {
  readonly layer: "browser";
  readonly events: readonly TerminalBrowserEvent[];
  /** terminal-stream-diagnostics projection; content-free replica state. */
  readonly replica: unknown;
  readonly trigger_state: TerminalBrowserPaintedState | null;
  readonly pre_repair_state: TerminalBrowserPaintedState | null;
  readonly post_repair_state: TerminalBrowserPaintedState | null;
  readonly current_state: TerminalBrowserPaintedState | null;
}

// ─── bundle ────────────────────────────────────────────────────────────────

export interface TerminalCaptureTrigger {
  readonly reason: TerminalCaptureReason;
  readonly origin: TerminalCaptureLayer;
  readonly at_ms: number;
  readonly stream_id: string | null;
  readonly grid_epoch: string | null;
  readonly seq: string | null;
  /** Fixed invariant token, never terminal text. */
  readonly detail: string | null;
  /** Further same-identity occurrences the latch collapsed. */
  readonly occurrence_count: number;
}

export interface TerminalIncidentBundle {
  readonly schema: typeof TERMINAL_INCIDENT_SCHEMA;
  readonly capture_id: string;
  readonly recording_id: string;
  readonly session_id: string;
  readonly written_at_ms: number;
  readonly trigger: TerminalCaptureTrigger;
  readonly coverage: TerminalCaptureCoverageReport;
  readonly browser: TerminalBrowserSection | null;
  readonly coordinator: TerminalCoordinatorSection | null;
  readonly worker: TerminalWorkerSection | null;
}
