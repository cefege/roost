// The controller contract: which W3C standard-mapping input produces which
// intent, which intents auto-repeat, and the legend text for each surface.
// Data only — no DOM, no Gamepad objects — so the mapper (lib/gamepadSource.ts)
// and the legend (components/PadHintBar.tsx) read one table instead of two.
// Callers: lib/gamepadSource.ts, lib/padActions.ts, components/PadHintBar.tsx.

export type PadAction =
	| "move-up" | "move-down" | "move-left" | "move-right"
	| "scroll-up" | "scroll-down"
	| "activate" | "back" | "palette" | "context-menu"
	| "tab-prev" | "tab-next" | "pane-prev" | "pane-next"
	| "keypad" | "help";

/** W3C standard-mapping button indices. Unlisted indices are unbound. */
export const PAD_BUTTON_ACTIONS: Readonly<Record<number, PadAction>> = {
	0: "activate", 1: "back", 2: "palette", 3: "context-menu",
	4: "tab-prev", 5: "tab-next", 6: "pane-prev", 7: "pane-next",
	8: "keypad", 9: "help",
	12: "move-up", 13: "move-down", 14: "move-left", 15: "move-right",
};

/** Actions that auto-repeat while held. Discrete commands never repeat. */
export const PAD_REPEATING_ACTIONS: Readonly<Partial<Record<PadAction, true>>> = {
	"move-up": true, "move-down": true, "move-left": true, "move-right": true,
	"scroll-up": true, "scroll-down": true,
};

export type PadHintContext = "default" | "menu" | "overlay" | "terminal";

export interface PadHint {
	readonly cap: string;
	readonly label: string;
}

// Xbox-style caps for every vendor: Gamepad.id strings are unreliable for
// vendor detection, and standard mapping fixes the indices regardless.
export const PAD_HINTS: Readonly<Record<PadHintContext, readonly PadHint[]>> = {
	default: [
		{ cap: "D-pad", label: "Move" }, { cap: "A", label: "Select" },
		{ cap: "B", label: "Back" }, { cap: "X", label: "Palette" },
		{ cap: "Y", label: "Menu" }, { cap: "LB/RB", label: "Tab" },
		{ cap: "LT/RT", label: "Pane" }, { cap: "Start", label: "Help" },
	],
	menu: [
		{ cap: "D-pad", label: "Move" }, { cap: "A", label: "Choose" },
		{ cap: "B", label: "Close" },
	],
	overlay: [
		{ cap: "D-pad", label: "Move" }, { cap: "A", label: "Open" },
		{ cap: "B", label: "Close" },
	],
	terminal: [
		{ cap: "D-pad", label: "Scroll" }, { cap: "R-stick", label: "Scroll" },
		{ cap: "A", label: "Keys" }, { cap: "B", label: "Back" },
		{ cap: "LB/RB", label: "Tab" }, { cap: "LT/RT", label: "Pane" },
	],
};
