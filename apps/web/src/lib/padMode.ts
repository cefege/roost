// Game-controller mode: the persisted switch that makes a standard-mapping pad
// a first-class input modality. Owns the choice, the "a pad actually sent input"
// latch, and the `data-pad` root attribute styles/gamepad.css keys its overrides
// off — so no component needs a controller branch. `?pad=1` / `?pad=0` is the
// bootstrap path for a console-attached browser with no reachable Settings.
//
// Callers: main.tsx (before first paint), Settings/ThemePane, lib/padActions,
// lib/gamepadSource, lib/directionalInput.
// Depends on: solid-js createSignal, @roost/shared/diag, localStorage, location.

import { createSignal } from "solid-js";
import { diag } from "@roost/shared/diag";

export type PadModeChoice = "auto" | "on" | "off";

const KEY = "roost.padMode";
const PARAM = "pad";

/** Read the persisted choice. `?pad=1` / `?pad=0` wins and is persisted, so the
 *  one-time URL typed on a console's on-screen keyboard survives later loads. */
export function loadPadModeChoice(): PadModeChoice {
	let fromUrl: PadModeChoice | null = null;
	try {
		const raw = new URLSearchParams(window.location.search).get(PARAM);
		if (raw === "1" || raw === "on" || raw === "true") fromUrl = "on";
		else if (raw === "0" || raw === "off" || raw === "false") fromUrl = "off";
		else if (raw === "auto") fromUrl = "auto";
	} catch {
		// no window/location (SSR, tests)
	}
	if (fromUrl) {
		try {
			localStorage.setItem(KEY, fromUrl);
		} catch {
			// localStorage disabled
		}
		return fromUrl;
	}
	try {
		const stored = localStorage.getItem(KEY);
		if (isChoice(stored)) return stored;
	} catch {
		// localStorage disabled
	}
	return "auto";
}

const [choiceSignal, setChoiceSignal] = createSignal<PadModeChoice>(
	loadPadModeChoice(),
);

/** Reactive accessor — the Settings picker highlights the selected choice. */
export const padModeChoice = choiceSignal;

// Latched by notePadActivity() on the first real button press or out-of-deadzone
// axis, never by a connection event.
const [padSeenSignal, setPadSeen] = createSignal(false);

export function padModeActive(): boolean {
	const choice = choiceSignal();
	if (choice === "on") return true;
	if (choice === "off") return false;
	return padSeenSignal();
}

/** Did a pad actually send input? `padModeActive()` deliberately conflates the
 *  latch with "forced on in Settings", so a surface that must appear ONLY for a
 *  real controller (the Start guide) has to read the latch itself. */
export function padInputSeen(): boolean {
	return padSeenSignal();
}

/** Latch `auto` on. A pad plugged in for games is a false positive: tvMode.ts
 *  records the incident where a weak heuristic switched real desktops into a
 *  D-pad UI that suppresses PTY focus, so mere connection must not flip the
 *  mode — only input the user deliberately produced does. */
export function notePadActivity(): void {
	if (padSeenSignal()) return;
	setPadSeen(true);
	applyPadMode();
}

/** Persist the choice and re-apply the root attribute immediately. */
export function setPadModeChoice(choice: PadModeChoice): void {
	try {
		localStorage.setItem(KEY, choice);
	} catch {
		// localStorage disabled
	}
	setChoiceSignal(choice);
	applyPadMode();
}

/** Write `data-pad` onto documentElement. Mirrors applyTvMode()'s root-attribute
 *  contract so all presentation state lands in one place before first paint. */
export function applyPadMode(): void {
	if (typeof document === "undefined") return;
	const active = padModeActive();
	document.documentElement.setAttribute("data-pad", active ? "true" : "false");
	diag("pad.mode", {
		choice: choiceSignal(),
		active,
		seen: padSeenSignal(),
	});
}

function isChoice(value: string | null): value is PadModeChoice {
	return value === "auto" || value === "on" || value === "off";
}
