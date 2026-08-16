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

import type { CellGridFrame } from "./types.ts";

/** Number of held viewport rows that a delta proves moved into scrollback.
 *
 * Scrollback can advance without changing the visible grid, so append length
 * alone is not a viewport-shift signal. A shift is reusable only when its first
 * appended row is exactly the former viewport head. Compare that one boundary
 * row directly: this neither hashes nor walks the full held viewport. */
export function deltaViewportShift(base: CellGridFrame, delta: CellGridFrame): number {
	const shift = Math.min(delta.scrollbackAppend.length, base.rows);
	if (shift === 0) return 0;
	const held = base.viewportRows[0];
	const appended = delta.scrollbackAppend[0];
	if (!held || !appended || held.spans.length !== appended.spans.length) return 0;
	for (let i = 0; i < held.spans.length; i++) {
		const a = held.spans[i]!;
		const b = appended.spans[i]!;
		if (
			a.text !== b.text
			|| a.fg !== b.fg
			|| a.bg !== b.bg
			|| a.flags !== b.flags
			|| a.fgRgb !== b.fgRgb
			|| a.bgRgb !== b.bgRgb
		) return 0;
	}
	return shift;
}

/** Apply a delta onto the client's held frame in place. Returns null when the
 * delta belongs to a different immutable grid epoch. A full frame replaces the
 * held frame wholesale. */
export function applyDelta(base: CellGridFrame, delta: CellGridFrame): CellGridFrame | null {
	if (delta.full) return delta;
	if (
		delta.gridEpoch !== base.gridEpoch
		|| delta.cols !== base.cols
		|| delta.rows !== base.rows
		|| delta.altScreen !== base.altScreen
		|| base.viewportRows.length !== base.rows
		|| delta.scrollbackRows.length !== 0
	) return null;

	const viewportRows = base.viewportRows;
	const scrolled = deltaViewportShift(base, delta);
	for (const row of delta.viewportRows) {
		if (!Number.isInteger(row.index) || row.index < 0 || row.index >= viewportRows.length) {
			return null;
		}
	}
	// A scroll shifts the canonical viewport just as the renderer shifts its DOM
	// nodes. Every newly exposed tail row must be present in the sparse patch;
	// otherwise the delta cannot reconstruct an authoritative grid.
	for (let index = base.rows - scrolled; index < base.rows; index++) {
		let supplied = false;
		for (const row of delta.viewportRows) {
			if (row.index === index) {
				supplied = true;
				break;
			}
		}
		if (!supplied) return null;
	}
	if (scrolled > 0) {
		viewportRows.copyWithin(0, scrolled);
		for (let index = 0; index < base.rows - scrolled; index++) {
			viewportRows[index]!.index = index;
		}
	}
	// Overwrite changed rows by index; every other row keeps its transferred
	// span array and, on the browser side, its existing DOM node.
	for (const row of delta.viewportRows) viewportRows[row.index] = row;

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
