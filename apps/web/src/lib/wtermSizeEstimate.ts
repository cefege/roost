// Best-effort estimate of the SPA's current wterm cols/rows for a new
// session spawn. The keeper reads ROOST_PTY_COLS/ROWS at PTY start so
// TUIs (claude, vim, …) paint to the correct width from byte 0 instead
// of the 220×50 keeper default. Without this, claude's full-redraw
// lands at 220 cols, then SIGWINCH-redraws after our resize message —
// the pre-resize paint leaves wrap/duplicate artifacts in the buffer.
//
// Re-measures on demand: spawn is rare enough that a fresh probe is
// fine, and we never want a stale cache after a window/zoom change.

function _liveDeckSlotSize(): { width: number; height: number } | null {
  // Prefer the visible deck slot if one is mounted — its rect is the
  // truthiest source. Falls back to the deck itself, then the cached
  // ResizeObserver value. Without this, the first + New click after a
  // page load (when no Terminal has mounted yet because the URL points
  // at a stale session) finds _lastSlot=null and ships no cols/rows.
  const deck = document.querySelector('[data-testid="terminal-deck"]') as HTMLElement | null;
  if (!deck) return null;
  for (const child of Array.from(deck.children) as HTMLElement[]) {
    if (getComputedStyle(child).visibility === "visible") {
      const r = child.getBoundingClientRect();
      if (r.width > 0 && r.height > 0) return { width: r.width, height: r.height };
    }
  }
  const r = deck.getBoundingClientRect();
  if (r.width > 0 && r.height > 0) return { width: r.width, height: r.height };
  return null;
}

export function estimateWtermSize(): { cols: number; rows: number } | null {
  const slot = _liveDeckSlotSize();
  if (!slot) return null;
  const { width, height } = slot;
  const probe = document.createElement("div");
  probe.className = "wterm";
  probe.style.cssText = "position:absolute;visibility:hidden;left:-9999px;top:-9999px";
  const grid = document.createElement("div");
  grid.className = "term-grid";
  const row = document.createElement("div");
  row.className = "term-row";
  const span = document.createElement("span");
  span.textContent = "XXXXXXXXXX";
  row.appendChild(span);
  grid.appendChild(row);
  probe.appendChild(grid);
  document.body.appendChild(probe);
  const rowRect = row.getBoundingClientRect();
  const spanRect = span.getBoundingClientRect();
  probe.remove();
  if (rowRect.height === 0 || spanRect.width === 0) return null;
  const cellW = spanRect.width / 10;
  const cellH = rowRect.height;
  const cols = Math.max(1, Math.floor(width / cellW));
  const rows = Math.max(1, Math.floor(height / cellH));
  return { cols, rows };
}
