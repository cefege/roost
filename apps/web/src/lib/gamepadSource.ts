// Gamepad API adapter: polls the first standard-mapping pad and turns held
// buttons and axes into PadActions with press/repeat semantics. The polling
// loop exists ONLY while a standard pad is connected and the modality is not
// off, so a desktop without one pays nothing. The mapper is pure and exported
// (`_` prefix) so the unit tier drives it without a browser.
// Callers: App.tsx (installGamepadSource(runPadActions)).
// Depends on: lib/padBindings, lib/padMode, @roost/shared/diag.

import { createEffect } from "solid-js";
import { diag } from "@roost/shared/diag";
import {
	PAD_BUTTON_ACTIONS,
	PAD_REPEATING_ACTIONS,
	type PadAction,
} from "./padBindings.ts";
import { notePadActivity, padModeChoice } from "./padMode.ts";

export const PAD_AXIS_DEADZONE = 0.35;
export const PAD_REPEAT_DELAY_MS = 400;
export const PAD_REPEAT_INTERVAL_MS = 110;

/** One poll's worth of pad state, free of Gamepad objects so the mapper is
 *  unit-testable without a browser. */
export interface PadSnapshot {
	readonly buttons: readonly boolean[];
	readonly axes: readonly number[];
}

/** Per-action hold state carried between polls: action → next-fire timestamp.
 *  A Map, not a Record — entries appear and vanish with every press. */
export type PadHoldState = Map<PadAction, number>;

// Axis index → [action past -deadzone, action past +deadzone]. Standard
// mapping: 0/1 = left stick X/Y, 2/3 = right stick X/Y. Axis 2 is unbound.
const AXIS_ACTIONS: Readonly<Record<number, readonly [PadAction, PadAction]>> = {
	0: ["move-left", "move-right"],
	1: ["move-up", "move-down"],
	3: ["scroll-up", "scroll-down"],
};

/** Actions to run for this poll: a fresh press, or a hold past its repeat gate.
 *  Mutates `holds` in place; exported for the unit test. */
export function _padActionsFromSnapshot(
	snapshot: PadSnapshot,
	holds: PadHoldState,
	nowMs: number,
): PadAction[] {
	const held: PadAction[] = [];
	for (let idx = 0; idx < snapshot.buttons.length; idx++) {
		if (!snapshot.buttons[idx]) continue;
		const action = PAD_BUTTON_ACTIONS[idx];
		if (action) held.push(action);
	}
	for (const [index, pair] of Object.entries(AXIS_ACTIONS)) {
		const value = snapshot.axes[Number(index)] ?? 0;
		if (value <= -PAD_AXIS_DEADZONE) held.push(pair[0]);
		else if (value >= PAD_AXIS_DEADZONE) held.push(pair[1]);
	}

	const fire: PadAction[] = [];
	for (const action of held) {
		const nextFireAt = holds.get(action);
		if (nextFireAt === undefined) {
			fire.push(action);
			// A non-repeating action fires once and never again while held; the
			// same press arriving from two inputs (D-pad + stick) dedupes here too.
			holds.set(
				action,
				PAD_REPEATING_ACTIONS[action] ? nowMs + PAD_REPEAT_DELAY_MS : Infinity,
			);
			continue;
		}
		if (nowMs >= nextFireAt) {
			fire.push(action);
			holds.set(action, nowMs + PAD_REPEAT_INTERVAL_MS);
		}
	}
	for (const action of holds.keys()) {
		if (!held.includes(action)) holds.delete(action);
	}
	return fire;
}

export function installGamepadSource(
	onActions: (actions: readonly PadAction[]) => void,
): () => void {
	if (installed)
		return () => {
			/* already installed */
		};
	installed = true;
	const holds: PadHoldState = new Map();
	let frame: number | null = null;
	let padCount = 0;

	const poll = (): void => {
		frame = requestAnimationFrame(poll);
		const pad = standardPads()[0];
		if (!pad) return;
		const actions = _padActionsFromSnapshot(
			{ buttons: pad.buttons.map((button) => button.pressed), axes: [...pad.axes] },
			holds,
			performance.now(),
		);
		if (actions.length === 0) return;
		// Arm `auto` BEFORE dispatching: the first press must both flip the
		// modality and act, so nothing is swallowed to "warm up" the mode.
		notePadActivity();
		onActions(actions);
	};

	const refreshPads = (): void => {
		const pads = standardPads().length;
		if ((pads === 0) !== (padCount === 0))
			diag("pad.connected", { pads, mapping: "standard" });
		padCount = pads;
		const shouldPoll = pads > 0 && padModeChoice() !== "off";
		if (shouldPoll && frame === null) frame = requestAnimationFrame(poll);
		else if (!shouldPoll && frame !== null) {
			cancelAnimationFrame(frame);
			frame = null;
			holds.clear();
		}
	};

	window.addEventListener("gamepadconnected", refreshPads);
	window.addEventListener("gamepaddisconnected", refreshPads);
	// Re-check on every choice change so flipping the Settings picker starts or
	// stops the loop at once. The install-time run also matters: after a reload
	// the pad is already connected and `gamepadconnected` may never fire again.
	createEffect(() => {
		padModeChoice();
		refreshPads();
	});

	return () => {
		installed = false;
		if (frame !== null) cancelAnimationFrame(frame);
		frame = null;
		window.removeEventListener("gamepadconnected", refreshPads);
		window.removeEventListener("gamepaddisconnected", refreshPads);
	};
}

let installed = false;

function standardPads(): Gamepad[] {
	try {
		return [...navigator.getGamepads()].filter(
			(pad): pad is Gamepad =>
				pad !== null && pad.connected && pad.mapping === "standard",
		);
	} catch {
		// no Gamepad API (older Safari, SSR, tests)
		return [];
	}
}
