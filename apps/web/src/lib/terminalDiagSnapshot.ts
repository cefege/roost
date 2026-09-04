// Terminal diagnostic snapshot — the browser half of the layered probe. Reads
// the registered CellGridRenderer plus the per-session replica/view owner and
// reports their independent stream watermarks in one bounded JSON record.
//
// Audience: diagnostics only. Every caller is a smoke-only module that is
// dynamically imported (lib/smoke.ts, lib/smokeHarness.ts), so nothing here is
// on the shipped terminal render path. The renderer registry itself lives in
// terminalPreview.ts (the tab-thumbnail feature) and stays there — this module
// reads it through rendererRegistryEntry() rather than keeping a second map.

import type {
  ReconcileBlockReason,
  RendererEpochSeq,
  RendererPresentationSnapshot,
} from "./cellRenderer.ts";
import { terminalStreamDiagnosticSnapshot } from "../store/terminal-stream-diagnostics.ts";
import type { TerminalStreamDiagnosticSnapshot } from "../store/terminal-stream-types.ts";
import { isPageVisible } from "./pageVisible.ts";
import type { ScrollbackHistoryFloor } from "@roost/shared/wire";
import { scrollbackHistoryFloor } from "./scrollbackBackfill.ts";
import { rendererRegistryEntry, type TerminalRendererOwnerSnapshot } from "./terminalPreview.ts";

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
  build: { git_sha: string | null };
  wire_received: TerminalStreamDiagnosticSnapshot["wire_received"];
  replica: TerminalStreamDiagnosticSnapshot["replica"];
  view: TerminalStreamDiagnosticSnapshot["view"];
  faults: TerminalStreamDiagnosticSnapshot["faults"];
  /** Retained name for adjacent probes; this is the browser replica watermark. */
  handler_canonical: RendererEpochSeq;
  dom_reconciled: RendererEpochSeq;
  reconcile_block_reason: ReconcileBlockReason;
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
    floor: { row: number; reason: ScrollbackHistoryFloor } | null;
  };
  last_geometry_proof: TerminalGeometryProof | null;
  slot: {
    registered: boolean;
    connected: boolean;
    in_layout: boolean | null;
    surface_active: boolean | null;
    css_visible: boolean | null;
  };
  visibility: TerminalRendererOwnerSnapshot["visibility"];
  sync: TerminalStreamDiagnosticSnapshot["sync"];
}

const buildShaValue = "VITE_BUILD_SHA" in import.meta.env
  ? import.meta.env.VITE_BUILD_SHA
  : undefined;
const BUILD_SHA = typeof buildShaValue === "string" ? buildShaValue : null;

/** Retain only the latest successful geometric proof for a mounted owner. */
export function recordTerminalGeometryProof(
  sessionId: string,
  proof: TerminalGeometryProof,
): void {
  if (proof.sessionId !== sessionId) return;
  const entry = rendererRegistryEntry(sessionId);
  if (!entry) return;
  entry.lastGeometryProof = proof.proof_kind === "marker"
    ? {
      ...proof,
      markerRect: { ...proof.markerRect },
      terminalRect: { ...proof.terminalRect },
      visualViewportRect: { ...proof.visualViewportRect },
    }
    : {
      ...proof,
      rect: { ...proof.rect },
      terminalClip: { ...proof.terminalClip },
      visualViewport: { ...proof.visualViewport },
    };
}

/** Stable bounded browser-local terminal state used both directly and as the
 * opaque SPA payload of the coordinator's layered diagnostic snapshot. */
export function terminalBrowserStreamSnapshot(sessionId: string): TerminalBrowserStreamSnapshot {
  const entry = rendererRegistryEntry(sessionId);
  const renderer = entry?.renderer ?? null;
  let owner: TerminalRendererOwnerSnapshot | null = null;
  try {
    owner = entry?.ownerSource?.() ?? null;
  } catch {
    // A diagnostic read must never perturb terminal ownership or rendering.
  }
  const stream = terminalStreamDiagnosticSnapshot(sessionId);
  // backfillAnchor is the SAME four values the paging controller addresses history
  // with, so the probe reports the range the browser is actually asking about
  // rather than a second derivation of it.
  const anchor = renderer?.backfillAnchor() ?? null;
  return {
    session_id: sessionId,
    captured_at_ms: Date.now(),
    build: { git_sha: BUILD_SHA },
    wire_received: stream.wire_received,
    replica: stream.replica,
    view: stream.view,
    faults: stream.faults,
    handler_canonical: {
      grid_epoch: stream.replica.grid_epoch,
      seq: stream.replica.seq,
    },
    dom_reconciled: renderer?.reconciledEpochSeq() ?? {
      grid_epoch: null,
      seq: null,
    },
    reconcile_block_reason: renderer?.reconcileBlockReason() ?? null,
    presentation: renderer?.presentationSnapshot() ?? null,
    history: {
      grid_epoch: anchor?.gridEpoch ?? null,
      sb_base: anchor?.sbBase ?? null,
      total: anchor?.total ?? null,
      cols: anchor?.cols ?? null,
      rows_held: renderer?.currentFrame?.scrollbackRows.length ?? 0,
      floor: scrollbackHistoryFloor(sessionId),
    },
    last_geometry_proof: entry?.lastGeometryProof ?? null,
    // View membership and replica continuity are reported above; renderer
    // ownership below is presentation-only.
    slot: {
      registered: entry !== undefined,
      connected: owner?.slot.connected ?? false,
      in_layout: owner?.slot.in_layout ?? null,
      surface_active: owner?.slot.surface_active ?? null,
      css_visible: owner?.slot.css_visible ?? null,
    },
    visibility: owner?.visibility ?? {
      document_visible: typeof document !== "undefined" && document.visibilityState === "visible",
      page_visible: isPageVisible(),
    },
    sync: stream.sync,
  };
}
