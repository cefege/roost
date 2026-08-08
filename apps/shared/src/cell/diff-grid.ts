// applyDelta(base, delta) → the reconstructed FULL frame (R11). The producing
// side is gridDeltaFrame (emitter.ts, core.isDirtyRow path) — its reframe
// rule (cols/rows change, alt-screen toggle, scrollback shrink → full frame)
// is what makes the delta safely applicable here. Round-trip against the
// real encoder is covered by tests/cell-realcore.test.ts.
//
// OWNERSHIP: the caller owns `base`; applyDelta CONSUMES and returns it. It
// mutates `base` in place rather than rebuilding, because rebuilding copied the
// whole held scrollback window (up to MAX_HELD_SCROLLBACK_ROWS = 2000 rows) on
// every frame that scrolled a single line. The only production caller is
// CellGridRenderer.apply, which holds exactly one frame per pane and always
// stores the return value back.

import type { CellRow, CellGridFrame } from "./types.ts";

/** Apply a delta onto the client's held frame in place. Returns null when the
 * delta belongs to a different immutable grid epoch. A full frame replaces the
 * held frame wholesale. */
export function applyDelta(base: CellGridFrame, delta: CellGridFrame): CellGridFrame | null {
	if (delta.full) return delta;
	if (delta.gridEpoch !== base.gridEpoch) return null;

	// A row-count change forces a full frame upstream, so this only ever fires
	// for a sparse or stale base (test fixtures, teardown races).
	const viewportRows = base.viewportRows;
	if (viewportRows.length !== delta.rows) {
		viewportRows.length = delta.rows;
		for (let row = 0; row < delta.rows; row++) {
			viewportRows[row] ??= { index: row, spans: [] } satisfies CellRow;
		}
	}
	// Overwrite changed rows by index; every other row keeps its held element,
	// which is also what lets renderViewport's hash diff skip them.
	for (const row of delta.viewportRows) {
		if (row.index >= 0 && row.index < viewportRows.length) viewportRows[row.index] = row;
	}

	// Scrollback is append-only, so the held history extends rather than copies.
	for (const row of delta.scrollbackAppend) base.scrollbackRows.push(row);
	if (base.scrollbackAppend.length > 0) base.scrollbackAppend.length = 0;

	base.gridEpoch = delta.gridEpoch;
	base.cols = delta.cols;
	base.rows = delta.rows;
	base.cursorRow = delta.cursorRow;
	base.cursorCol = delta.cursorCol;
	base.cursorVisible = delta.cursorVisible;
	base.altScreen = delta.altScreen;
	base.cursorKeysApp = delta.cursorKeysApp;
	base.bracketedPaste = delta.bracketedPaste;
	base.full = true;
	base.scrollbackTotal = delta.scrollbackTotal;
	base.seq = delta.seq;
	// sbBase is deliberately untouched: deltas never move the held window's base.
	return base;
}
