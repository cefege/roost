// Terminal preview — renders real terminal text into a DOM container by
// reading the CellGridRenderer's in-memory frame. Used by MobileDeckBar's
// TerminalCard to show what the terminal has been doing in the tab grid
// (like Chrome's tab thumbnails — but text, not a rasterized image).
//
// No live streaming: the snapshot is taken once when the card mounts (sheet
// opens). Every open session's CellTerminal stays mounted by TerminalDeck
// (parked off-screen), so its CellGridRenderer always holds the latest frame.
//
// Cost: O(rows × spans) — a trivial walk, no emulator re-run, no re-render.

import { spanStyle, rowText, type CellGridRenderer } from "./cellRenderer.ts";

// ─── Renderer registry ──────────────────────────────────────────────────
// Mirrors sync-dispatch.ts's per-session handler Map. CellTerminal registers
// its renderer on mount so a global renderPreview(sessionId) can resolve it.

const _renderers = new Map<string, CellGridRenderer>();

export function registerRenderer(sessionId: string, r: CellGridRenderer): () => void {
  _renderers.set(sessionId, r);
  return () => { if (_renderers.get(sessionId) === r) _renderers.delete(sessionId); };
}

/** The live CellGridRenderer for a session, or undefined. */
export function getRenderer(sessionId: string): CellGridRenderer | undefined {
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
  const renderer = _renderers.get(sessionId);
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
    if (rowText(row).trim() !== "") picked.push(row);
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
