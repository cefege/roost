// Sparse cell-delta batch folding for one browser paint.
// Reuses the single-delta validator and owns mutable row coordinates.
// Callers receive one canonical successor plus final-coordinate patch rows.

import {
	applyDelta,
	cloneCellGridFrame,
	deltaViewportShift,
} from "./diff-grid.ts";
import type { CellGridFrame, CellRow } from "./types.ts";

export interface CellDeltaBatch {
	/** Independently owned canonical successor after every input delta. */
	frame: CellGridFrame;
	/** Rows to patch in the successor's final viewport coordinates. */
	dirtyRows: readonly CellRow[];
	/** Newly appended history rows in their original arrival order. */
	scrollbackAppend: readonly CellRow[];
	/** Proven viewport movement from the starting frame, capped to its height. */
	viewportShift: number;
}

function cloneRows(rows: readonly CellRow[]): CellRow[] {
	return rows.map((row) => ({ index: row.index, spans: row.spans }));
}

/** A delta can donate viewport and appended-history row shells to the fold. */
function ownDeltaRows(delta: CellGridFrame): CellGridFrame {
	return {
		...delta,
		viewportRows: cloneRows(delta.viewportRows),
		scrollbackAppend: cloneRows(delta.scrollbackAppend),
	};
}

/** Delta history is append-only: a gap would make one batched DOM append lie. */
function hasContiguousScrollbackAppend(base: CellGridFrame, delta: CellGridFrame): boolean {
	if (
		!Number.isSafeInteger(base.scrollbackTotal)
		|| base.scrollbackTotal < 0
		|| !Number.isSafeInteger(delta.scrollbackTotal)
		|| delta.scrollbackTotal !== base.scrollbackTotal + delta.scrollbackAppend.length
	) return false;

	for (let offset = 0; offset < delta.scrollbackAppend.length; offset++) {
		if (delta.scrollbackAppend[offset]?.index !== base.scrollbackTotal + offset) return false;
	}
	return true;
}

/**
 * Fold contiguous sparse deltas into one independently owned canonical frame.
 * Invalid chains never mutate the caller's baseline or any supplied delta.
 */
export function foldCellDeltaBatch(
	base: CellGridFrame,
	deltas: readonly CellGridFrame[],
): CellDeltaBatch | null {
	if (deltas.length === 0) return null;

	const folded = cloneCellGridFrame(base);
	if (!Number.isSafeInteger(folded.rows) || folded.rows < 0) return null;

	const dirtyMarks = new Uint8Array(folded.rows);
	const scrollbackAppend: CellRow[] = [];
	let viewportShift = 0;

	for (const delta of deltas) {
		if (delta.full || !hasContiguousScrollbackAppend(folded, delta)) return null;

		const ownedDelta = ownDeltaRows(delta);
		const shift = deltaViewportShift(folded, ownedDelta);
		if (!applyDelta(folded, ownedDelta)) return null;

		if (shift > 0) {
			dirtyMarks.copyWithin(0, shift);
			dirtyMarks.fill(0, folded.rows - shift);
		}
		for (const row of ownedDelta.viewportRows) dirtyMarks[row.index] = 1;
		for (const row of ownedDelta.scrollbackAppend) scrollbackAppend.push(row);
		viewportShift = Math.min(folded.rows, viewportShift + shift);
	}

	const dirtyRows: CellRow[] = [];
	if (viewportShift === folded.rows) {
		dirtyRows.push(...folded.viewportRows);
	} else {
		for (let index = 0; index < folded.rows; index++) {
			if (dirtyMarks[index] !== 0) dirtyRows.push(folded.viewportRows[index]!);
		}
	}

	return { frame: folded, dirtyRows, scrollbackAppend, viewportShift };
}
