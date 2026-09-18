// Pins the clamp that decides whether a controller direction scrolls the pane
// or leaves it: the return value IS the "can this box still travel" predicate
// padActions' move-up/move-down read before handing the direction to focus
// navigation. A writer that reported success at an edge would dead-end the pad
// inside the terminal with no way out.
//
// DOM-free: the writer touches only scrollTop/scrollHeight/clientHeight.

import { describe, test, expect } from "bun:test";
import {
	PAD_SCROLL_STEP_PX,
	scrollTerminalReaderBox,
} from "../src/lib/terminalReaderScroll.ts";

function fakeBox(scrollTop: number, scrollHeight: number, clientHeight: number) {
	return { scrollTop, scrollHeight, clientHeight } as unknown as HTMLElement;
}

describe("scrollTerminalReaderBox", () => {
	test("reports no travel and writes nothing at either edge", () => {
		const atTop = fakeBox(0, 1000, 400);
		expect(scrollTerminalReaderBox(atTop, -PAD_SCROLL_STEP_PX)).toBe(false);
		expect(atTop.scrollTop).toBe(0);

		const atBottom = fakeBox(600, 1000, 400);
		expect(scrollTerminalReaderBox(atBottom, PAD_SCROLL_STEP_PX)).toBe(false);
		expect(atBottom.scrollTop).toBe(600);
	});

	test("clamps an overshoot to the scroll maximum", () => {
		const box = fakeBox(550, 1000, 400);
		expect(scrollTerminalReaderBox(box, PAD_SCROLL_STEP_PX)).toBe(true);
		expect(box.scrollTop).toBe(600);
	});

	test("moves by the full step inside the range", () => {
		const box = fakeBox(200, 1000, 400);
		expect(scrollTerminalReaderBox(box, -PAD_SCROLL_STEP_PX)).toBe(true);
		expect(box.scrollTop).toBe(200 - PAD_SCROLL_STEP_PX);
	});

	test("reports no travel when the box has no scroll range", () => {
		const box = fakeBox(0, 400, 400);
		expect(scrollTerminalReaderBox(box, PAD_SCROLL_STEP_PX)).toBe(false);
		expect(box.scrollTop).toBe(0);
	});
});
