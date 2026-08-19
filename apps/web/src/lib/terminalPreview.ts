// Terminal preview — renders real terminal text into a DOM container by
// reading the CellGridRenderer's in-memory frame. Used by MobileDeckBar's
// TerminalCard to show what the terminal has been doing in the tab grid
// (like Chrome's tab thumbnails — but text, not a rasterized image).
//
// No live streaming: the snapshot is taken once when the card mounts (sheet
// opens). TerminalDeck keeps a BOUNDED set of warm sessions' CellTerminals
// mounted (parked off-screen, lib/deckWarmSet.ts) — and the deck host survives
// /file/… and /search as a pure visibility flip (MainPane) — so a still-warm
// session's CellGridRenderer always holds its latest frame. A session never
// warmed this page-load, or evicted from the warm set, renders no preview.
//
// Cost: O(rows × spans) — a trivial walk, no emulator re-run, no re-render.

import type { CellGridRenderer, RendererEpochSeq } from "./cellRenderer.ts";
import { spanStyle } from "./cellRow.ts";
import { spansText } from "@roost/shared/cell";
// Type-only back-reference: terminalDiagSnapshot.ts owns the geometry-proof
// shape; this registry only retains the latest one per mounted owner, so the
// proof lifetime stays tied to the renderer entry it was measured against.
import type { TerminalGeometryProof } from "./terminalDiagSnapshot.ts";

// ─── Renderer registry ──────────────────────────────────────────────────
// Mirrors sync-dispatch.ts's per-session handler Map. CellTerminal registers
// its renderer on mount so a global renderPreview(sessionId) can resolve it.

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

type RendererOwnerSource = () => TerminalRendererOwnerSnapshot;

export interface RendererRegistryEntry {
  renderer: CellGridRenderer;
  ownerSource: RendererOwnerSource | null;
  lastGeometryProof: TerminalGeometryProof | null;
}

const _renderers = new Map<string, RendererRegistryEntry>();

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

/** Narrow handle for terminalDiagSnapshot.ts, which reports the registered
 * renderer's watermarks and retains its latest geometry proof. The registry
 * stays single-sourced here instead of being duplicated there. */
export function rendererRegistryEntry(sessionId: string): RendererRegistryEntry | undefined {
  return _renderers.get(sessionId);
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
