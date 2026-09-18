// Gamepad API adapter: polls the first standard-mapping pad, turns held buttons
// and axes into PadActions with press/repeat semantics, and publishes the live
// held state the controller map highlights. The polling loop exists ONLY while
// a standard pad is connected and the modality is not off, so a desktop without
// one pays nothing. The mapper is pure and exported (`_` prefix) for the tests.
// Callers: App.tsx (installGamepadSource(runPadActions)), ControllerMap.tsx.
// Depends on: lib/padBindings, lib/padMode, @roost/shared/diag.

import { createEffect, createSignal } from "solid-js";
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

const [heldButtons, setHeldButtons] = createSignal<ReadonlySet<number>>(new Set());
const [heldActions, setHeldActions] = createSignal<ReadonlySet<PadAction>>(new Set());

/** Standard-mapping indices held right now, INCLUDING unbound ones: seeing 10
 *  and 11 light up on the controller map is how a user learns whether their pad
 *  reports the stick clicks this build binds the mic and folders to. */
export const padHeldButtons = heldButtons;

/** Intents held right now. The map highlights by intent, so a D-pad press and
 *  a stick push light the same row. */
export const padHeldActions = heldActions;

// Bound axis → [index, action past -deadzone, action past +deadzone]. Standard
// mapping: 0/1 = left stick X/Y, 2/3 = right stick X/Y. Axis 2 is unbound.
// A list, not a Record: its order also fixes the held-signature bit positions.
const AXIS_ACTIONS: readonly (readonly [number, PadAction, PadAction])[] = [
	[0, "move-left", "move-right"],
	[1, "move-up", "move-down"],
	[3, "scroll-up", "scroll-down"],
];

/** Every intent held in this snapshot, before any press/repeat gating — the
 *  controller map highlights what is held, not what fired. */
export function _heldPadActions(snapshot: PadSnapshot): PadAction[] {
	const held: PadAction[] = [];
	for (let idx = 0; idx < snapshot.buttons.length; idx++) {
		if (!snapshot.buttons[idx]) continue;
		const action = PAD_BUTTON_ACTIONS[idx];
		if (action) held.push(action);
	}
	for (const [index, negative, positive] of AXIS_ACTIONS) {
		const value = snapshot.axes[index] ?? 0;
		if (value <= -PAD_AXIS_DEADZONE) held.push(negative);
		else if (value >= PAD_AXIS_DEADZONE) held.push(positive);
	}
	return held;
}

/** Publish held state for the live controller diagram; null when the loop
 *  stops. The signature compare is the point: a fresh Set every frame would
 *  re-render the whole map at 60 fps, and allocating one to discover it was
 *  unchanged would burn a poll's worth of garbage per frame besides. */
export function _publishPadHeld(snapshot: PadSnapshot | null): void {
	const signature = snapshot ? heldSignature(snapshot) : 0;
	if (signature === publishedSignature) return;
	publishedSignature = signature;
	const buttons = new Set<number>();
	if (snapshot)
		for (let idx = 0; idx < snapshot.buttons.length; idx++)
			if (snapshot.buttons[idx]) buttons.add(idx);
	setHeldButtons(buttons);
	setHeldActions(new Set(snapshot ? _heldPadActions(snapshot) : []));
}

/** Actions to run for this poll: a fresh press, or a hold past its repeat gate.
 *  Mutates `holds` in place; exported for the unit test. */
export function _padActionsFromSnapshot(
	snapshot: PadSnapshot,
	holds: PadHoldState,
	nowMs: number,
): PadAction[] {
	const held = _heldPadActions(snapshot);

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
		if (!pad) {
			_publishPadHeld(null);
			return;
		}
		const snapshot: PadSnapshot = {
			buttons: pad.buttons.map((button) => button.pressed),
			axes: [...pad.axes],
		};
		_publishPadHeld(snapshot);
		const actions = _padActionsFromSnapshot(snapshot, holds, performance.now());
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
			_publishPadHeld(null);
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
		_publishPadHeld(null);
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

let publishedSignature = 0;

// One integer per distinct held state: a bit per button index, then two bits
// per bound axis for its deadzone crossings. Buttons past the bit budget are
// still published, they just cannot trigger a republish alone — standard
// mapping defines 17 (0-16, where 16 is the optional guide button).
const SIGNATURE_BUTTON_BITS = 24;

function heldSignature(snapshot: PadSnapshot): number {
	let bits = 0;
	const counted = Math.min(snapshot.buttons.length, SIGNATURE_BUTTON_BITS);
	for (let idx = 0; idx < counted; idx++)
		if (snapshot.buttons[idx]) bits |= 1 << idx;
	for (let axis = 0; axis < AXIS_ACTIONS.length; axis++) {
		const value = snapshot.axes[AXIS_ACTIONS[axis][0]] ?? 0;
		if (value <= -PAD_AXIS_DEADZONE) bits |= 1 << (SIGNATURE_BUTTON_BITS + axis * 2);
		else if (value >= PAD_AXIS_DEADZONE) bits |= 1 << (SIGNATURE_BUTTON_BITS + axis * 2 + 1);
	}
	return bits;
}
