// Mouse reporting encoder — the ONE place a browser pointer/touch gesture turns
// into the bytes the FOREGROUND application asked for. Both inputs come from the
// worker's cell frame (CellGridFrame.mouseTracking / .mouseSgr, read off
// @wterm/core's DECSET state), never from alt-screen occupancy: vim, less and man
// occupy the alt screen without ever requesting mouse reporting, and forwarding
// to them swallowed the click with no native fallback.
//
// Pure and synchronous by design: the decision (forward or leave it native) and
// the encoding are one function, so the whole matrix — mode, gesture kind,
// held-button state, modifier bypass, both wire formats — is unit-testable
// without a DOM, a pane, or a session.
//
// Two wire formats, because the application picks:
//   * SGR-1006 (DECSET 1006): `ESC [ < Cb ; Cx ; Cy M` for press/motion/wheel,
//     final `m` for release. Decimal, so coordinates are unbounded.
//   * legacy X10 (no 1006): `ESC [ M` then three BYTES, each value + 32. A
//     coordinate tops out at 223 (xterm's MOUSE_LIMIT, 255 - 32), so the byte
//     itself runs to 255 and a far column reports as 223 rather than saturating
//     the byte early — clamping the BYTE at 223 would misreport every column
//     past 191, which ordinary Roost panes reach. Bytes past 0x7f are also why
//     this returns a Uint8Array: a string would be UTF-8 encoded on the way out
//     and silently double each one. Release carries no button identity in X10:
//     it reports 3.

import type { MouseTracking } from "@roost/shared/cell";

/** Cb bits. Shift and Alt are deliberately absent: they are Roost's per-gesture
 *  bypass to native selection, so they never reach the application at all. */
const CB_META = 8;
const CB_CTRL = 16;
const CB_MOTION = 32;
const CB_WHEEL_UP = 64;
const CB_WHEEL_DOWN = 65;
/** X10 has no per-button release; every release reports "all buttons up". */
const CB_X10_RELEASE = 3;

const X10_BIAS = 32;
/** xterm's MOUSE_LIMIT: the largest coordinate the biased byte can carry. */
const X10_MAX_CELL = 255 - X10_BIAS;

const ESC = 0x1b;
const LEFT_BRACKET = 0x5b;
const CAPITAL_M = 0x4d;

const ENCODER = new TextEncoder();

/** The two mode bits the frame carries: what tracking the app requested and
 *  which encoding it wants for it. */
export interface MouseReportModes {
	tracking: MouseTracking;
	sgr: boolean;
}

export interface MouseGestureModifiers {
	shift?: boolean;
	alt?: boolean;
	ctrl?: boolean;
	meta?: boolean;
}

interface MouseGestureCell extends MouseGestureModifiers {
	/** 1-based grid cell under the pointer, as the terminal numbers them. */
	col: number;
	row: number;
}

/** `button` is the DOM MouseEvent.button (0 left, 1 middle, 2 right); `held` is
 *  whether a button is still down, which is the whole difference between mode
 *  1000 and 1002 for a motion. */
export type MouseGesture =
	| (MouseGestureCell & { kind: "press" | "release"; button: number })
	| (MouseGestureCell & { kind: "motion"; button: number; held: boolean })
	| (MouseGestureCell & { kind: "wheelUp" | "wheelDown" });

/** Bytes to send the PTY for this gesture, or null when it must stay native.
 *
 *  null means "the browser keeps it": the app requested no tracking, or the
 *  gesture is one this mode does not report (motion outside 1002, motion with no
 *  button held), or Shift/Alt asked for native selection instead. Shift/Alt is
 *  consulted only for a gesture that STARTS a forwarded interaction (press,
 *  wheel) — abandoning an in-flight drag halfway would leave the application
 *  holding a button it never sees released. */
export function terminalMouseReport(
	modes: MouseReportModes,
	gesture: MouseGesture,
): Uint8Array | null {
	if (modes.tracking === 0) return null;

	let cb: number;
	let release = false;
	switch (gesture.kind) {
		case "wheelUp":
		case "wheelDown":
			if (gesture.shift === true || gesture.alt === true) return null;
			cb = gesture.kind === "wheelUp" ? CB_WHEEL_UP : CB_WHEEL_DOWN;
			break;
		case "press":
			if (gesture.shift === true || gesture.alt === true) return null;
			if (gesture.button > 2 || gesture.button < 0) return null;
			cb = gesture.button;
			break;
		case "release":
			if (gesture.button > 2 || gesture.button < 0) return null;
			cb = modes.sgr ? gesture.button : CB_X10_RELEASE;
			release = true;
			break;
		case "motion":
			// 1000 reports presses only; 1002 adds motion, but strictly while a
			// button is held (any-motion 1003 is folded to 0 by the core, so a
			// hover is never reportable).
			if (modes.tracking !== 1002 || !gesture.held) return null;
			if (gesture.button > 2 || gesture.button < 0) return null;
			cb = gesture.button | CB_MOTION;
			break;
	}
	if (gesture.meta === true) cb |= CB_META;
	if (gesture.ctrl === true) cb |= CB_CTRL;

	const col = Math.max(1, Math.trunc(gesture.col));
	const row = Math.max(1, Math.trunc(gesture.row));
	if (modes.sgr) return ENCODER.encode(`\x1b[<${cb};${col};${row}${release ? "m" : "M"}`);
	// Cb needs no bound: the widest value here is a ctrl+meta wheel notch (89).
	return new Uint8Array([
		ESC, LEFT_BRACKET, CAPITAL_M,
		cb + X10_BIAS,
		Math.min(col, X10_MAX_CELL) + X10_BIAS,
		Math.min(row, X10_MAX_CELL) + X10_BIAS,
	]);
}

/** The painted grid's geometry, in client (viewport) pixel space.
 *
 *  `left`/`top` are the origin of CELL (1,1) — the top-left of the PAINTED row
 *  box, NOT of the scroll container: the scrollback sheet and the history
 *  spacer sit above the rows inside that container, so the container's top is
 *  hundreds of pixels off in any pane with history. */
export interface TerminalCellGeometry {
	left: number;
	top: number;
	cellWidth: number;
	rowHeight: number;
	cols: number;
	rows: number;
}

/** Which cell a client-space point lands on, 1-based, clamped INTO the grid.
 *
 *  Clamping is not cosmetic: a pane letterboxes (the rows are pinned to
 *  cols × 1ch, so a wider pane has margin) and the last row rarely ends exactly
 *  at the container's bottom, so an unclamped report hands the application a
 *  column past `cols` or a row past `rows` — which real TUIs mishandle rather
 *  than ignore. A gesture in the margin belongs to the nearest edge cell. */
export function cellFromPoint(
	geometry: TerminalCellGeometry,
	clientX: number,
	clientY: number,
): { col: number; row: number } {
	const col = 1 + Math.floor((clientX - geometry.left) / geometry.cellWidth);
	const row = 1 + Math.floor((clientY - geometry.top) / geometry.rowHeight);
	return {
		col: Math.min(geometry.cols, Math.max(1, col)),
		row: Math.min(geometry.rows, Math.max(1, row)),
	};
}
