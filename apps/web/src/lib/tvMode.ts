// TV mode: the one switch that turns the SPA into a ten-foot, D-pad-driven UI.
// Owns the persisted choice, the TV-browser heuristic, and the `data-tv` root
// attribute that styles/tv.css keys every override off — so no component needs
// a TV branch. `?tv=1` / `?tv=0` is the bootstrap path, because Settings is
// unreachable from an unpaired TV.
//
// Callers: main.tsx (before first paint), Settings/ThemePane, keyboardShortcuts,
// spatialNavigation, CellTerminal + its interaction/renderer helpers.
// Depends on: solid-js createSignal, @roost/shared/diag, localStorage, location.

import { createSignal } from "solid-js";
import { diag } from "@roost/shared/diag";

export type TvModeChoice = "auto" | "on" | "off";

const KEY = "roost.tvMode";
const PARAM = "tv";
// Vendor tokens observed in smart-TV / set-top user agents. A TV missing from
// this list that also reports a pointer is a false negative the user fixes with
// ?tv=1 — widening the media query instead would catch real desktops.
const TV_UA =
	/\b(smart-?tv|smarttv|googletv|android\s?tv|appletv|crkey|hbbtv|netcast|web0s|webos|tizen|viera|aquos|bravia|philipstv|roku|nettv|dtv)\b/i;
// Below this width a "(pointer: none)" match is far more likely a phone with a
// broken media-query implementation than a television.
const TV_MIN_POINTERLESS_WIDTH_PX = 1280;

/** Read the persisted choice. `?tv=1` / `?tv=0` wins and is persisted, so the
 *  one-time URL typed on a TV's on-screen keyboard survives every later load. */
export function loadTvModeChoice(): TvModeChoice {
	let fromUrl: TvModeChoice | null = null;
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

const [choiceSignal, setChoiceSignal] = createSignal<TvModeChoice>(
	loadTvModeChoice(),
);

/** Reactive accessor — the Settings picker highlights the selected choice. */
export const tvModeChoice = choiceSignal;

/** UA match only. Not reactive: a TV never becomes a laptop, and re-evaluating
 *  per render would make every consumer re-measure.
 *
 *  A pointerless wide viewport is deliberately NOT sufficient: automation
 *  browsers and pointer-less desktops report `(pointer: none)` at any width, so
 *  that branch silently switched real desktops into a D-pad UI that suppresses
 *  PTY focus and makes the terminal unusable. `?tv=1` remains the escape hatch
 *  for a television this UA list misses. */
export function detectTvBrowser(): boolean {
	return matchesTvUserAgent();
}

export function tvModeActive(): boolean {
	const choice = choiceSignal();
	if (choice === "on") return true;
	if (choice === "off") return false;
	return detectTvBrowser();
}

/** Persist the choice and re-apply the root attribute immediately. */
export function setTvModeChoice(choice: TvModeChoice): void {
	try {
		localStorage.setItem(KEY, choice);
	} catch {
		// localStorage disabled
	}
	setChoiceSignal(choice);
	applyTvMode();
}

/** Write `data-tv` onto documentElement. Mirrors applyTheme()'s root-attribute
 *  contract so all presentation state lands in one place before first paint. */
export function applyTvMode(): void {
	if (typeof document === "undefined") return;
	const active = tvModeActive();
	document.documentElement.setAttribute("data-tv", active ? "true" : "false");
	diag("tv.mode", {
		choice: choiceSignal(),
		active,
		ua_match: matchesTvUserAgent(),
		pointer_none: matchesPointerlessViewport(),
	});
}

// Both probes are cached: tvModeActive() runs on every keydown through
// spatialNavigation, and a TV never becomes a laptop mid-session, so re-running
// matchMedia() and the UA regex per keystroke buys nothing.
let uaMatch: boolean | null = null;
let pointerlessMatch: boolean | null = null;

function matchesTvUserAgent(): boolean {
	if (uaMatch === null) {
		try {
			uaMatch = TV_UA.test(navigator.userAgent);
		} catch {
			uaMatch = false;
		}
	}
	return uaMatch;
}

function matchesPointerlessViewport(): boolean {
	if (pointerlessMatch === null) {
		try {
			pointerlessMatch =
				window.matchMedia("(pointer: none)").matches &&
				window.innerWidth >= TV_MIN_POINTERLESS_WIDTH_PX;
		} catch {
			pointerlessMatch = false;
		}
	}
	return pointerlessMatch;
}

function isChoice(value: string | null): value is TvModeChoice {
	return value === "auto" || value === "on" || value === "off";
}

