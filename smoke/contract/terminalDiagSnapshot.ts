// Type surface of the page's terminal diagnostic snapshot and presentation
// proofs, as `window.__smoke` returns them: the per-session stream
// watermarks, route, renderer presentation and the geometric proofs a paint
// probe resolves with. Type-only; the page's producer is the roost-web smoke
// module, and specs read these shapes through smokeTypes.ts and the probes.

export interface TerminalRectSnapshot {
  left: number;
  top: number;
  right: number;
  bottom: number;
  width: number;
  height: number;
}

export interface MarkerPresentationProof {
  proof_kind: "marker";
  sessionId: string;
  marker: string;
  monotonicMs: number;
  epochMs: number;
  rowText: string;
  markerRect: TerminalRectSnapshot;
  terminalRect: TerminalRectSnapshot;
  visualViewportRect: TerminalRectSnapshot;
  frames: 2;
}

export interface CursorPresentationProof {
  proof_kind: "cursor";
  sessionId: string;
  row: number;
  column: number;
  monotonicMs: number;
  epochMs: number;
  rect: TerminalRectSnapshot;
  terminalClip: TerminalRectSnapshot;
  visualViewport: TerminalRectSnapshot;
  frames: 2;
}

export type TerminalGeometryProof = MarkerPresentationProof | CursorPresentationProof;

export interface TerminalBrowserStreamSnapshot {
  session_id: string;
  captured_at_ms: number;
  build: {
    git_sha: string | null;
  };
  wire_received: {
    stream_id: string | null;
    grid_epoch: string | null;
    seq: number | null;
  };
  replica: {
    expected_stream_id: string | null;
    grid_epoch: string | null;
    seq: number | null;
    baseline_ready: boolean;
    resync_latched: boolean;
    last_terminal_proof_age_ms: number | null;
    last_terminal_proof_generation: TerminalGenerationDiagnosticToken | null;
    challenge_age_ms: number | null;
    challenge_generation: TerminalGenerationDiagnosticToken | null;
    challenge_stream_id: string | null;
    challenge_seq: number | null;
    resync_latch_age_ms: number | null;
    resync_latch_generation: TerminalGenerationDiagnosticToken | null;
    repair_attempts: number;
    repair_outcome: TerminalRepairOutcome;
  };
  view: {
    view_id: string | null;
    revision: string | null;
    active: boolean;
    status: "pending" | "accepted" | "unavailable" | "rejected" | null;
    stream_id: string | null;
    effective_cols: number | null;
    effective_rows: number | null;
    lease_deadline_ms: number | null;
    pending_ack_age_ms: number | null;
    pending_ack_generation: TerminalGenerationDiagnosticToken | null;
  };
  faults: {
    blackhole_drop_count: number;
    wire_delta_drop_count: number;
    wire_delta_dropped_seq: number | null;
    wire_delta_post_drop_seq: number | null;
  };
  /** Retained name for adjacent probes; this is the browser replica watermark. */
  handler_canonical: RendererEpochSeq;
  dom_reconciled: RendererEpochSeq;
  reconcile_block_reason: ReconcileBlockReason;
  route: TerminalRouteDiagnosticSnapshot;
  presentation: RendererPresentationSnapshot | null;
  /** The range THIS document holds, for comparison against the worker's core and
   *  ring ranges in the same layered probe. `sb_base`/`total` are the frame's own
   *  absolute bounds, `rows_held` the scrollback rows actually in the model, and
   *  `floor` the point above which paging stopped plus WHY it stopped there —
   *  genuine eviction or a resize-bounded replay. null floor = no page has come
   *  back short in this epoch, so nothing is known to be missing. */
  history: {
    grid_epoch: string | null;
    sb_base: number | null;
    total: number | null;
    cols: number | null;
    rows_held: number;
    floor: {
      row: number;
      reason: "none" | "evicted" | "resize_replay";
    } | null;
  };
  last_geometry_proof: TerminalGeometryProof | null;
  slot: {
    registered: boolean;
    connected: boolean;
    in_layout: boolean | null;
    surface_active: boolean | null;
    css_visible: boolean | null;
  };
  visibility: {
    document_visible: boolean;
    page_visible: boolean;
  };
  sync: {
    socket_generation: number | null;
    socket_id: string | null;
    process_epoch: string | null;
    domain_generation: string | null;
    ready: boolean;
  };
}

export interface TerminalGenerationDiagnosticToken {
  readonly socketGeneration: number;
  readonly socketId: string;
  readonly processEpoch: string;
  readonly domainGeneration: string;
  readonly transportKind: TerminalTransportKind;
  readonly workerFp: string | null;
}

export type TerminalTransportKind = "sync" | "loopback" | "webrtc";

export type TerminalRepairOutcome = "none" | "requested" | "proved" | "escalated" | "generation_reset" | "inactive" | "disposed" | "stream_replaced" | "pruned";

export interface TerminalRouteDiagnosticSnapshot {
  active: TerminalRouteDiagnosticEntry | null;
  candidate: TerminalRouteDiagnosticEntry | null;
  peer_phase: PeerPhase | null;
  fallback_reason: FallbackReason;
  failure_detail: string | null;
  input_phase: TerminalInputPhase | null;
  pending_input_count: number;
}

export interface TerminalRouteDiagnosticEntry {
  kind: TerminalTransportKind;
  worker_epoch: string | null;
  peer_id: string | null;
  phase: "candidate" | "active";
  candidate_type: TerminalPeerCandidateType;
  probe_age_ms: number | null;
  rtt_ms: number | null;
  /** Content-free control round-trip to the selected worker, not a browser ping. */
  worker_control_rtt_ms: number | null;
  buffered_bytes: number | null;
}

export type TerminalPeerCandidateType = "none" | "host" | "srflx" | "prflx";

export type PeerPhase = "idle" | "grant" | "gathering" | "negotiating" | "authenticating" | "candidate" | "active" | "cooldown" | "disabled";

export type FallbackReason = "disabled" | "unsupported" | "cap" | "network_failed" | "coordinator_unavailable" | "invalid_response" | "ice_failed" | null;

export type TerminalInputPhase = "closed" | "sending" | "holding" | "claiming" | "blocked";

export interface RendererEpochSeq {
  grid_epoch: string | null;
  seq: number | null;
}

export type ReconcileBlockReason = "reader_pending_frame" | "selection_hold" | "link_hold" | "selection_and_link_hold" | "pending_render" | "not_reconciled" | null;

export interface RendererPresentationSnapshot {
  captured_at_ms: number;
  canonical: RendererEpochSeq;
  reconciled: RendererEpochSeq;
  reader_intent: ReaderIntent;
  reader_reason: ReaderIntentReason | null;
  hold_mask: {
    selection: boolean;
    link: boolean;
  };
  rows: {
    canonical: number | null;
    dom: number;
  };
  mode: {
    canonical: RendererTerminalModeSnapshot | null;
    reconciled: RendererTerminalModeSnapshot | null;
  };
  cursor: {
    canonical: {
      visible: boolean;
      row: number;
      column: number;
    } | null;
    dom: {
      visible: boolean | null;
      row: number | null;
      column: number | null;
      connected: boolean;
    };
  };
  cols: {
    canonical: number | null;
    dom: number | null;
  };
  at_bottom: boolean;
  follows_bottom: boolean;
}

export type ReaderIntent = "live" | "reading";

export type ReaderIntentReason = "find" | "native_scroll" | "wheel" | "touch" | "selection";

export interface RendererTerminalModeSnapshot {
  alt_screen: boolean;
  cursor_keys_app: boolean;
  bracketed_paste: boolean;
}

export interface RendererPaintPresentation {
  rows: {
    index: number;
    text: string;
  }[];
  headSpacerPx: number;
  tailGapPx: number;
  readerAnchor: ReaderAnchor | null;
}

export interface ReaderAnchor {
  row: number;
  offsetPx: number;
}
