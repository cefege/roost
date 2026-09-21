// Pins the geometry rule a TV remote depends on: an arrow press must move focus
// to the control the user is pointing at, not merely to the nearest one. The
// scorer weighs off-axis drift twice as hard as travel distance, so a control
// almost straight ahead beats a closer one far to the side.
//
// DOM-free: this repo has no browser-DOM harness, so candidates are objects
// carrying only the getBoundingClientRect the scorer reads.

import { describe, test, expect } from "bun:test";
import {
	_bestCandidateInDirection,
	_isExcludedSpatialCandidate,
} from "../src/lib/spatialNavigation.ts";

function fakeRect(left: number, top: number, width: number, height: number): DOMRect {
	return {
		left,
		top,
		width,
		height,
		right: left + width,
		bottom: top + height,
		x: left,
		y: top,
	} as DOMRect;
}

function fakeElement(rect: DOMRect): HTMLElement {
	return { getBoundingClientRect: () => rect } as unknown as HTMLElement;
}

function fakeFocusability(
	terminalInput = false,
	spatialNavigation?: string,
): Pick<HTMLElement, "classList" | "dataset"> {
	return {
		classList: {
			contains: (className: string) => terminalInput && className === "terminal-input",
		},
		dataset: spatialNavigation === undefined ? {} : { spatialNavigation },
	} as Pick<HTMLElement, "classList" | "dataset">;
}

// Origin sits mid-screen: left 100, top 100, right 200, bottom 150.
const origin = fakeRect(100, 100, 100, 50);

describe("_bestCandidateInDirection", () => {
	test("each arrow picks the control on that side of the origin", () => {
		const above = fakeElement(fakeRect(100, 0, 100, 50));
		const below = fakeElement(fakeRect(100, 300, 100, 50));
		const toLeft = fakeElement(fakeRect(0, 100, 50, 50));
		const toRight = fakeElement(fakeRect(400, 100, 100, 50));
		const all = [above, below, toLeft, toRight];

		expect(_bestCandidateInDirection(origin, all, "up")).toBe(above);
		expect(_bestCandidateInDirection(origin, all, "down")).toBe(below);
		expect(_bestCandidateInDirection(origin, all, "left")).toBe(toLeft);
		expect(_bestCandidateInDirection(origin, all, "right")).toBe(toRight);
	});

	test("a nearer but far off-axis candidate loses to a farther on-axis one", () => {
		const straightDown = fakeElement(fakeRect(100, 300, 100, 50));
		const nearerButSideways = fakeElement(fakeRect(900, 200, 100, 50));

		expect(
			_bestCandidateInDirection(origin, [nearerButSideways, straightDown], "down"),
		).toBe(straightDown);
	});

	test("returns null when nothing lies in the requested direction", () => {
		const above = fakeElement(fakeRect(100, 0, 100, 50));

		expect(_bestCandidateInDirection(origin, [above], "down")).toBeNull();
		expect(_bestCandidateInDirection(origin, [], "up")).toBeNull();
	});

	test("a candidate overlapping the origin edge is not a move", () => {
		// Centre inside the origin's own band: travelling there would not advance
		// focus, so the scorer must reject it rather than re-focus in place.
		const overlapping = fakeElement(fakeRect(100, 120, 100, 20));

		expect(_bestCandidateInDirection(origin, [overlapping], "down")).toBeNull();
		expect(_bestCandidateInDirection(origin, [overlapping], "up")).toBeNull();
	});
});

describe("_isExcludedSpatialCandidate", () => {
	test("keeps only the exact manual marker out of directional focus", () => {
		expect(_isExcludedSpatialCandidate(fakeFocusability(false, "manual"))).toBe(true);
		expect(_isExcludedSpatialCandidate(fakeFocusability(false, "auto"))).toBe(false);
		expect(_isExcludedSpatialCandidate(fakeFocusability())).toBe(false);
	});

	test("continues excluding the terminal input", () => {
		expect(_isExcludedSpatialCandidate(fakeFocusability(true))).toBe(true);
	});
});
