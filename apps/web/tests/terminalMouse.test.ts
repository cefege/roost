// Decision matrix for the mouse-report encoder. Every row is a gate the browser
// used to get wrong by encoding SGR unconditionally whenever a pane happened to
// occupy the alt screen: whether the app asked at all, whether THIS mode reports
// THIS gesture, which of the two wire formats it asked for, and whether the user
// asked for native selection instead for one gesture.

import { describe, expect, test } from "bun:test";
import {
	terminalMouseReport,
	type MouseGesture,
	type MouseGestureModifiers,
	type MouseReportModes,
} from "../src/lib/terminalMouse.ts";

const SGR: MouseReportModes = { tracking: 1002, sgr: true };
const SGR_CLICK: MouseReportModes = { tracking: 1000, sgr: true };
const X10: MouseReportModes = { tracking: 1002, sgr: false };
const OFF: MouseReportModes = { tracking: 0, sgr: true };

const text = (bytes: Uint8Array | null): string | null =>
	bytes === null ? null : new TextDecoder().decode(bytes);

function press(
	over: MouseGestureModifiers & { button?: number; col?: number; row?: number } = {},
): MouseGesture {
	return { kind: "press", button: 0, col: 1, row: 1, ...over };
}

describe("terminalMouseReport", () => {
	test("an app that never requested tracking gets nothing, whatever the gesture", () => {
		const gestures: MouseGesture[] = [
			{ kind: "press", button: 0, col: 4, row: 2 },
			{ kind: "release", button: 0, col: 4, row: 2 },
			{ kind: "motion", button: 0, held: true, col: 4, row: 2 },
			{ kind: "wheelUp", col: 4, row: 2 },
			{ kind: "wheelDown", col: 4, row: 2 },
		];
		for (const gesture of gestures) expect(terminalMouseReport(OFF, gesture)).toBeNull();
		expect(terminalMouseReport({ tracking: 0, sgr: false }, press())).toBeNull();
	});

	test("mode 1000 reports press and release but never motion", () => {
		expect(text(terminalMouseReport(SGR_CLICK, { kind: "press", button: 0, col: 7, row: 3 })))
			.toBe("\x1b[<0;7;3M");
		expect(text(terminalMouseReport(SGR_CLICK, { kind: "release", button: 0, col: 7, row: 3 })))
			.toBe("\x1b[<0;7;3m");
		expect(terminalMouseReport(SGR_CLICK, { kind: "motion", button: 0, held: true, col: 8, row: 3 }))
			.toBeNull();
	});

	test("mode 1002 reports motion only while a button is held", () => {
		expect(text(terminalMouseReport(SGR, { kind: "motion", button: 0, held: true, col: 8, row: 3 })))
			.toBe("\x1b[<32;8;3M");
		expect(text(terminalMouseReport(SGR, { kind: "motion", button: 2, held: true, col: 8, row: 3 })))
			.toBe("\x1b[<34;8;3M");
		// A hover: mode 1003 (any-motion) is folded to 0 by the core, so an unheld
		// move is never reportable in any mode Roost can see.
		expect(terminalMouseReport(SGR, { kind: "motion", button: 0, held: false, col: 8, row: 3 }))
			.toBeNull();
	});

	test("buttons and modifier bits land in Cb, and Shift/Alt keep the gesture native", () => {
		expect(text(terminalMouseReport(SGR, press({ button: 2, col: 3, row: 4 })))).toBe("\x1b[<2;3;4M");
		expect(text(terminalMouseReport(SGR, press({ meta: true })))).toBe("\x1b[<8;1;1M");
		expect(text(terminalMouseReport(SGR, press({ ctrl: true })))).toBe("\x1b[<16;1;1M");
		expect(text(terminalMouseReport(SGR, press({ ctrl: true, meta: true })))).toBe("\x1b[<24;1;1M");
		// Shift/Alt are Roost's bypass, so they neither forward nor set bit 4.
		expect(terminalMouseReport(SGR, press({ shift: true }))).toBeNull();
		expect(terminalMouseReport(SGR, press({ alt: true }))).toBeNull();
		expect(terminalMouseReport(SGR, { kind: "wheelUp", col: 1, row: 1, shift: true })).toBeNull();
		expect(terminalMouseReport(SGR, { kind: "wheelDown", col: 1, row: 1, alt: true })).toBeNull();
		// Mid-drag the app already owns the button: a modifier pressed after the
		// press must not strand it holding a button it never sees released.
		expect(text(terminalMouseReport(SGR, {
			kind: "motion", button: 0, held: true, col: 2, row: 2, shift: true,
		}))).toBe("\x1b[<32;2;2M");
		expect(text(terminalMouseReport(SGR, {
			kind: "release", button: 0, col: 2, row: 2, alt: true,
		}))).toBe("\x1b[<0;2;2m");
	});

	test("wheel notches are buttons 64 and 65 in both encodings", () => {
		expect(text(terminalMouseReport(SGR, { kind: "wheelUp", col: 1, row: 1 }))).toBe("\x1b[<64;1;1M");
		expect(text(terminalMouseReport(SGR, { kind: "wheelDown", col: 1, row: 1 }))).toBe("\x1b[<65;1;1M");
		expect(text(terminalMouseReport(SGR_CLICK, { kind: "wheelDown", col: 2, row: 5 })))
			.toBe("\x1b[<65;2;5M");
		expect(terminalMouseReport(X10, { kind: "wheelUp", col: 1, row: 1 }))
			.toEqual(new Uint8Array([0x1b, 0x5b, 0x4d, 96, 33, 33]));
		expect(terminalMouseReport(X10, { kind: "wheelDown", col: 1, row: 1 }))
			.toEqual(new Uint8Array([0x1b, 0x5b, 0x4d, 97, 33, 33]));
	});

	test("without DECSET 1006 the report is legacy X10 bytes, release included", () => {
		expect(terminalMouseReport(X10, { kind: "press", button: 0, col: 1, row: 1 }))
			.toEqual(new Uint8Array([0x1b, 0x5b, 0x4d, 32, 33, 33]));
		expect(terminalMouseReport(X10, { kind: "press", button: 2, col: 10, row: 4 }))
			.toEqual(new Uint8Array([0x1b, 0x5b, 0x4d, 34, 42, 36]));
		expect(terminalMouseReport(X10, { kind: "motion", button: 0, held: true, col: 10, row: 4 }))
			.toEqual(new Uint8Array([0x1b, 0x5b, 0x4d, 64, 42, 36]));
		// X10 has no per-button release: every release is "all buttons up" (3).
		expect(terminalMouseReport(X10, { kind: "release", button: 2, col: 10, row: 4 }))
			.toEqual(new Uint8Array([0x1b, 0x5b, 0x4d, 35, 42, 36]));
		expect(terminalMouseReport(X10, { kind: "press", button: 0, col: 1, row: 1, ctrl: true }))
			.toEqual(new Uint8Array([0x1b, 0x5b, 0x4d, 48, 33, 33]));
	});

	test("X10 coordinates clamp at cell 223 while SGR stays exact", () => {
		// xterm's MOUSE_LIMIT: cell 223 is the last one the biased byte can name,
		// and it names it as 255. Clamping the BYTE at 223 instead would collapse
		// every column past 191 onto 191 — inside the width of an ordinary pane.
		expect(terminalMouseReport(X10, { kind: "press", button: 0, col: 191, row: 200 }))
			.toEqual(new Uint8Array([0x1b, 0x5b, 0x4d, 32, 223, 232]));
		expect(terminalMouseReport(X10, { kind: "press", button: 0, col: 223, row: 223 }))
			.toEqual(new Uint8Array([0x1b, 0x5b, 0x4d, 32, 255, 255]));
		expect(terminalMouseReport(X10, { kind: "press", button: 0, col: 400, row: 300 }))
			.toEqual(new Uint8Array([0x1b, 0x5b, 0x4d, 32, 255, 255]));
		// Every byte stays one byte: a string would UTF-8 expand everything past 0x7f.
		expect(terminalMouseReport(X10, { kind: "press", button: 0, col: 400, row: 300 })!.length).toBe(6);
		expect(text(terminalMouseReport(SGR, { kind: "press", button: 0, col: 400, row: 300 })))
			.toBe("\x1b[<0;400;300M");
	});

	test("buttons with no mouse-report encoding are left alone", () => {
		// Back/forward (DOM 3/4) have no Cb in either format; middle has one and is
		// excluded at the call site for the deck's bring-to-front gesture instead.
		expect(terminalMouseReport(SGR, press({ button: 3 }))).toBeNull();
		expect(terminalMouseReport(SGR, press({ button: 4 }))).toBeNull();
		expect(terminalMouseReport(SGR, { kind: "release", button: 3, col: 1, row: 1 })).toBeNull();
		expect(text(terminalMouseReport(SGR, press({ button: 1 })))).toBe("\x1b[<1;1;1M");
	});
});
