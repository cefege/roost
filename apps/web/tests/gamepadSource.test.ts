// Pins the press/repeat contract a controller depends on: a button must fire
// once on press, a direction must auto-repeat only after the initial delay, a
// discrete command must never repeat while held, and a stick inside the deadzone
// must not fire at all. Those four rules are the whole difference between "one
// nudge moves one row" and "one nudge scrolls the pane away".
//
// DOM-free: the mapper takes a plain PadSnapshot, never a Gamepad object.

import { describe, test, expect } from "bun:test";
import {
	_padActionsFromSnapshot,
	PAD_REPEAT_DELAY_MS,
	PAD_REPEAT_INTERVAL_MS,
	type PadHoldState,
	type PadSnapshot,
} from "../src/lib/gamepadSource.ts";

const BUTTON_DOWN = 13;
const BUTTON_A = 0;
const LEFT_STICK_Y = 1;

function padSnapshot(
	pressed: readonly number[],
	axes: readonly number[] = [0, 0, 0, 0],
): PadSnapshot {
	return {
		buttons: Array.from({ length: 16 }, (_unused, idx) => pressed.includes(idx)),
		axes,
	};
}

describe("_padActionsFromSnapshot", () => {
	test("a held direction fires once, then repeats after the delay", () => {
		const holds: PadHoldState = new Map();
		const down = padSnapshot([BUTTON_DOWN]);

		expect(_padActionsFromSnapshot(down, holds, 0)).toEqual(["move-down"]);
		expect(_padActionsFromSnapshot(down, holds, 100)).toEqual([]);
		expect(_padActionsFromSnapshot(down, holds, PAD_REPEAT_DELAY_MS)).toEqual([
			"move-down",
		]);
		expect(
			_padActionsFromSnapshot(
				down,
				holds,
				PAD_REPEAT_DELAY_MS + PAD_REPEAT_INTERVAL_MS,
			),
		).toEqual(["move-down"]);
	});

	test("a held discrete command fires exactly once", () => {
		const holds: PadHoldState = new Map();
		const activate = padSnapshot([BUTTON_A]);

		expect(_padActionsFromSnapshot(activate, holds, 0)).toEqual(["activate"]);
		expect(_padActionsFromSnapshot(activate, holds, PAD_REPEAT_DELAY_MS)).toEqual([]);
		expect(
			_padActionsFromSnapshot(activate, holds, PAD_REPEAT_DELAY_MS * 10),
		).toEqual([]);
	});

	test("a stick inside the deadzone produces nothing", () => {
		const holds: PadHoldState = new Map();
		const axes = [0, 0, 0, 0];

		axes[LEFT_STICK_Y] = -0.3;
		expect(_padActionsFromSnapshot(padSnapshot([], axes), holds, 0)).toEqual([]);

		axes[LEFT_STICK_Y] = -0.4;
		expect(_padActionsFromSnapshot(padSnapshot([], axes), holds, 0)).toEqual([
			"move-up",
		]);
	});

	test("releasing clears the hold so the next press fires immediately", () => {
		const holds: PadHoldState = new Map();
		const down = padSnapshot([BUTTON_DOWN]);

		expect(_padActionsFromSnapshot(down, holds, 0)).toEqual(["move-down"]);
		expect(_padActionsFromSnapshot(padSnapshot([]), holds, 10)).toEqual([]);
		expect(holds.size).toBe(0);
		expect(_padActionsFromSnapshot(down, holds, 20)).toEqual(["move-down"]);
	});
});
