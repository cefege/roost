// Terminal preview — renders real terminal text into a DOM container by
// reading the CellGridRenderer's in-memory frame. Used by MobileDeckBar's
// TerminalCard to show what the terminal has been doing in the tab grid
// (like Chrome's tab thumbnails — but text, not a rasterized image).
//
// No live streaming: the snapshot is taken once when the card mounts (sheet
// opens). TerminalDeck keeps every WARM session's CellTerminal mounted (parked
// off-screen) for the life of the page — the deck host survives /file/… and
// /search as a pure visibility flip (MainPane) — so a warmed session's
// CellGridRenderer always holds its latest frame. A session never yet warmed
// this page-load has no renderer and renders no preview.
//
// Cost: O(rows × spans) — a trivial walk, no emulator re-run, no re-render.

import type {
  CellGridRenderer,
  ReconcileBlockReason,
  RendererEpochSeq,
  RendererPresentationSnapshot,
} from "./cellRenderer.ts";
import { cellWireEpochSeq } from "../store/sync-dispatch.ts";
import { terminalOutboundSnapshot, type TerminalOutboundSnapshot } from "../ws/sync-outbound.ts";
import { isPageVisible } from "./pageVisible.ts";
import { spanStyle } from "./cellRow.ts";
import { spansText } from "@roost/shared/cell";
import type { ScrollbackHistoryFloor } from "@roost/shared/wire";
import { scrollbackHistoryFloor } from "./scrollbackBackfill.ts";

// ─── Renderer registry ──────────────────────────────────────────────────
// Mirrors sync-dispatch.ts's per-session handler Map. CellTerminal registers
// its renderer on mount so a global renderPreview(sessionId) can resolve it.

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

export interface TerminalRendererOwnerSnapshot {
  handler_canonical: RendererEpochSeq;
  slot: {
    connected: boolean;
    in_layout: boolean | null;
    surface_active: boolean | null;
    css_visible: boolean | null;
  };
  visibility: {
    document_visible: boolean;
    page_visible: boolean;
  };
}

export interface TerminalBrowserStreamSnapshot {
  session_id: string;
  captured_at_ms: number;
  build: { git_sha: string | null };
  wire_received: RendererEpochSeq;
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
  claim: TerminalOutboundSnapshot["claim"];
  slot: {
    registered: boolean;
    connected: boolean;
    in_layout: boolean | null;
    surface_active: boolean | null;
    css_visible: boolean | null;
  };
  visibility: TerminalRendererOwnerSnapshot["visibility"];
  sync: TerminalOutboundSnapshot["sync"];
}

type RendererOwnerSource = () => TerminalRendererOwnerSnapshot;

interface RendererRegistryEntry {
  renderer: CellGridRenderer;
  ownerSource: RendererOwnerSource | null;
  lastGeometryProof: TerminalGeometryProof | null;
}

const _renderers = new Map<string, RendererRegistryEntry>();
const buildShaValue = "VITE_BUILD_SHA" in import.meta.env
  ? import.meta.env.VITE_BUILD_SHA
  : undefined;
const BUILD_SHA = typeof buildShaValue === "string" ? buildShaValue : null;

export function registerRenderer(
  sessionId: string,
  renderer: CellGridRenderer,
  ownerSource?: RendererOwnerSource,
): () => void {
  const entry: RendererRegistryEntry = {
    renderer,
    ownerSource: ownerSource ?? null,
    lastGeometryProof: null,
  };
  _renderers.set(sessionId, entry);
  return () => {
    if (_renderers.get(sessionId) === entry) _renderers.delete(sessionId);
  };
}

/** Retain only the latest successful geometric proof for a mounted owner. */
export function recordTerminalGeometryProof(
  sessionId: string,
  proof: TerminalGeometryProof,
): void {
  if (proof.sessionId !== sessionId) return;
  const entry = _renderers.get(sessionId);
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
  const entry = _renderers.get(sessionId);
  const renderer = entry?.renderer ?? null;
  let owner: TerminalRendererOwnerSnapshot | null = null;
  try {
    owner = entry?.ownerSource?.() ?? null;
  } catch {
    // A diagnostic read must never perturb terminal ownership or rendering.
  }
  const outbound = terminalOutboundSnapshot(sessionId);
  // backfillAnchor is the SAME four values the paging controller addresses history
  // with, so the probe reports the range the browser is actually asking about
  // rather than a second derivation of it.
  const anchor = renderer?.backfillAnchor() ?? null;
  return {
    session_id: sessionId,
    captured_at_ms: Date.now(),
    build: { git_sha: BUILD_SHA },
    wire_received: cellWireEpochSeq(sessionId),
    handler_canonical: owner?.handler_canonical ?? { grid_epoch: null, seq: null },
    dom_reconciled: renderer?.reconciledEpochSeq() ?? { grid_epoch: null, seq: null },
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
    claim: outbound.claim,
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
    sync: outbound.sync,
  };
}


// ─── Preview renderer ───────────────────────────────────────────────────

/** Max non-blank rows to show in the preview (fits the 160px card area). */
const MAX_PREVIEW_ROWS = 18;
/** Candidate rows to scan (scrollback tail + viewport). */
const CANDIDATE_SCAN = 20;

/**
 * Render the current terminal content for `sessionId` into `container` as
 * styled DOM text. Returns false if no renderer/frame is available or no
 * non-blank rows exist (caller shows faux fallback).
 *
 * Collects the newest non-blank rows from the frame's scrollback tail + viewport
 * (alt-screen sessions have empty scrollback — viewport alone is the content),
 * and paints each span with the existing `spanStyle()` so terminal colors and
 * text attributes (bold, dim, underline, etc.) are preserved verbatim.
 */
export function renderPreview(sessionId: string, container: HTMLElement): boolean {
  const renderer = _renderers.get(sessionId)?.renderer;
  const frame = renderer?.currentFrame;
  if (!frame || frame.viewportRows.length === 0) return false;

  // Candidate rows: recent scrollback + viewport (newest at the end).
  const candidates = [
    ...frame.scrollbackRows.slice(-CANDIDATE_SCAN),
    ...frame.viewportRows,
  ];

  // Collect up to MAX_PREVIEW_ROWS non-blank rows, newest first.
  const picked: typeof candidates = [];
  for (let i = candidates.length - 1; i >= 0 && picked.length < MAX_PREVIEW_ROWS; i--) {
    const row = candidates[i]!;
    if (spansText(row.spans).trim() !== "") picked.push(row);
  }
  if (picked.length === 0) return false;

  // Render oldest→newest (top→bottom).
  picked.reverse();

  container.replaceChildren();
  const doc = container.ownerDocument;
  for (const row of picked) {
    const rowEl = doc.createElement("div");
    rowEl.className = "terminal-card-preview-row";
    if (row.spans.length === 0) {
      // Blank-looking row that had non-trim whitespace — keep it tall.
      rowEl.appendChild(doc.createTextNode("\u00a0"));
    } else {
      for (const span of row.spans) {
        const spanEl = doc.createElement("span");
        spanEl.setAttribute("style", spanStyle(span));
        spanEl.textContent = span.text;
        rowEl.appendChild(spanEl);
      }
    }
    container.appendChild(rowEl);
  }

  return true;
}
