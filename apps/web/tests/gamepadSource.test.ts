// Pins the press/repeat contract a controller depends on: a button must fire
// once on press, a direction must auto-repeat only after the initial delay, a
// discrete command must never repeat while held, and a stick inside the deadzone
// must not fire at all. Those four rules are the whole difference between "one
// nudge moves one row" and "one nudge scrolls the pane away". Also pins the
// held-state publication the live controller map highlights from.
//
// DOM-free: the mapper takes a plain PadSnapshot, never a Gamepad object.

import { beforeEach, describe, test, expect } from "bun:test";
import {
	_padActionsFromSnapshot,
	_publishPadHeld,
	padHeldActions,
	padHeldButtons,
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
	buttonCount = 16,
): PadSnapshot {
	return {
		buttons: Array.from({ length: buttonCount }, (_unused, idx) => pressed.includes(idx)),
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

describe("_publishPadHeld", () => {
	beforeEach(() => _publishPadHeld(null));

	test("publishes raw indices, unbound ones included", () => {
		// Index 16 is standard mapping's optional guide button and this build
		// binds nothing to it: the map still lights it, because seeing a button
		// register is how a user learns their pad reports it at all.
		_publishPadHeld(padSnapshot([10, 11, 16], [0, 0, 0, 0], 17));

		expect([...padHeldButtons()]).toEqual([10, 11, 16]);
		expect([...padHeldActions()]).toEqual(["mic-toggle", "folder-next"]);
	});

	test("rewrites the sets only when membership changes", () => {
		_publishPadHeld(padSnapshot([13]));
		const held = padHeldButtons();

		// Same membership from a fresh snapshot object: a new Set here would
		// re-render the whole controller map every frame the button is down.
		_publishPadHeld(padSnapshot([13]));
		expect(padHeldButtons()).toBe(held);

		_publishPadHeld(padSnapshot([13, 0]));
		expect(padHeldButtons()).not.toBe(held);
		expect([...padHeldButtons()]).toEqual([0, 13]);
	});

	test("an axis past the deadzone publishes its intent, release clears it", () => {
		_publishPadHeld(padSnapshot([], [0, 0, 0, -1]));
		expect([...padHeldActions()]).toEqual(["scroll-up"]);

		_publishPadHeld(padSnapshot([], [0, 0, 0, -0.1]));
		expect(padHeldActions().size).toBe(0);
	});

	test("a stopped poll loop clears the held state", () => {
		_publishPadHeld(padSnapshot([0], [-1, 0, 0, 0]));
		expect(padHeldActions().size).toBe(2);

		_publishPadHeld(null);
		expect(padHeldButtons().size).toBe(0);
		expect(padHeldActions().size).toBe(0);
	});
});
