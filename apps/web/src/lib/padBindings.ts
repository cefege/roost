// The controller contract: which W3C standard-mapping input produces which
// intent, what each physical control does in words, and which intents repeat
// while held. Data only — no DOM, no Gamepad objects — so the mapper
// (lib/gamepadSource.ts), the transient legend (components/PadHintBar.tsx) and
// the controller map (components/ControllerMap.tsx) read ONE table, not three.
// Callers: lib/gamepadSource.ts, lib/padActions.ts, PadHintBar, ControllerMap.

export type PadAction =
	| "move-up" | "move-down" | "move-left" | "move-right"
	| "scroll-up" | "scroll-down"
	| "activate" | "back" | "palette" | "context-menu"
	| "tab-prev" | "tab-next" | "pane-prev" | "pane-next"
	| "keypad" | "controller-map" | "mic-toggle" | "folder-next";

/** W3C standard-mapping button indices. Unlisted indices are unbound. */
export const PAD_BUTTON_ACTIONS: Readonly<Record<number, PadAction>> = {
	0: "activate", 1: "back", 2: "palette", 3: "context-menu",
	4: "tab-prev", 5: "tab-next", 6: "pane-prev", 7: "pane-next",
	8: "keypad", 9: "controller-map", 10: "mic-toggle", 11: "folder-next",
	12: "move-up", 13: "move-down", 14: "move-left", 15: "move-right",
};

/** Actions that auto-repeat while held. Discrete commands never repeat — a
 *  held mic or folder button that fired every frame would be unusable. */
export const PAD_REPEATING_ACTIONS: Readonly<Partial<Record<PadAction, true>>> = {
	"move-up": true, "move-down": true, "move-left": true, "move-right": true,
	"scroll-up": true, "scroll-down": true,
};

/** One physical control, in words. `action` is null for the analogue clusters,
 *  which produce a direction family rather than one intent. */
export interface PadControlGuide {
	readonly cap: string;
	readonly action: PadAction | null;
	/** Legend verb — the PAD_HINTS vocabulary. */
	readonly label: string;
	/** One clause spelling out the context-dependent behaviour. */
	readonly detail: string;
}

export type PadHintContext =
	| "default" | "menu" | "overlay" | "terminal" | "keypad" | "dictation";

export interface PadHint {
	readonly cap: string;
	readonly label: string;
}

// Xbox-style caps for every vendor: Gamepad.id strings are unreliable for
// vendor detection, and standard mapping fixes the indices regardless. This is
// the ONLY place a control's wording lives — PAD_HINTS derives its caps and
// default verbs from these rows, so a legend that contradicts the controller
// map cannot ship. `cap` is also the controller map's geometry key.
export const PAD_CONTROL_GUIDE: readonly PadControlGuide[] = [
	{
		cap: "A", action: "activate", label: "Select",
		detail: "Select the focused item; sends the dictation while recording; on a focused terminal it opens the key pad and lands on the first key",
	},
	{
		cap: "B", action: "back", label: "Back",
		detail: "Go back, close, or leave the terminal; discards the dictation while recording; closes the key pad and hands focus back to the terminal",
	},
	{
		cap: "X", action: "palette", label: "Palette",
		detail: "Toggle the command palette",
	},
	{
		cap: "Y", action: "context-menu", label: "Menu",
		detail: "Open the context menu for the focused item",
	},
	{
		cap: "LB", action: "tab-prev", label: "Tab",
		detail: "Previous tab in the focused pane, cycling",
	},
	{
		cap: "RB", action: "tab-next", label: "Tab",
		detail: "Next tab in the focused pane, cycling",
	},
	{
		cap: "LT", action: "pane-prev", label: "Pane",
		detail: "Previous pane in the layout, cycling",
	},
	{
		cap: "RT", action: "pane-next", label: "Pane",
		detail: "Next pane in the layout, cycling",
	},
	{
		cap: "Back", action: "keypad", label: "Keys",
		detail: "Toggle the terminal key pad; opening it focuses the first key so the pad can walk the keys",
	},
	{
		cap: "Start", action: "controller-map", label: "Guide",
		detail: "Toggle this controller guide",
	},
	{
		cap: "L3", action: "mic-toggle", label: "Mic",
		detail: "Click the left stick to start dictation, or to stop and send it; the browser may need one tap of the on-screen mic first to grant the microphone",
	},
	{
		cap: "R3", action: "folder-next", label: "Folder",
		detail: "Click the right stick to switch to the next folder, cycling by recent activity",
	},
	{
		cap: "D-pad", action: null, label: "Move",
		detail: "Move focus, or scroll a focused terminal until it reaches the edge",
	},
	{
		cap: "L-stick", action: null, label: "Move",
		detail: "Move focus like the D-pad, repeating while held",
	},
	{
		cap: "R-stick", action: null, label: "Scroll",
		detail: "Scroll the focused pane's terminal scrollback",
	},
];

export const PAD_HINTS: Readonly<Record<PadHintContext, readonly PadHint[]>> = {
	default: [
		hintFor(["D-pad"]), hintFor(["A"]), hintFor(["B"]), hintFor(["X"]),
		hintFor(["Y"]), hintFor(["LB", "RB"]), hintFor(["LT", "RT"]),
		hintFor(["L3"]), hintFor(["R3"]), hintFor(["Start"]),
	],
	menu: [hintFor(["D-pad"]), hintFor(["A"], "Choose"), hintFor(["B"], "Close")],
	overlay: [hintFor(["D-pad"]), hintFor(["A"], "Open"), hintFor(["B"], "Close")],
	terminal: [
		hintFor(["D-pad"], "Scroll"), hintFor(["R-stick"]), hintFor(["A"], "Keys"),
		hintFor(["B"]), hintFor(["LB", "RB"]), hintFor(["LT", "RT"]),
		hintFor(["L3"]), hintFor(["R3"]), hintFor(["Start"]),
	],
	keypad: [hintFor(["D-pad"]), hintFor(["A"], "Press"), hintFor(["B"], "Close")],
	dictation: [
		hintFor(["A"], "Send"), hintFor(["B"], "Discard"), hintFor(["L3"], "Stop"),
	],
};

// ── private ─────────────────────────────────────────────────────────────────

/** A legend row for one or more controls: caps joined the way a user reads them
 *  ("LB/RB"), verb defaulting to the guide's so a context spells out only what
 *  it genuinely changes. Throws on an unknown cap — a typo would otherwise ship
 *  a legend row the controller map has no control for. */
function hintFor(caps: readonly string[], label?: string): PadHint {
	const rows = caps.map((cap) => {
		const row = PAD_CONTROL_GUIDE.find((candidate) => candidate.cap === cap);
		if (!row) throw new Error(`pad hint references unknown control ${cap}`);
		return row;
	});
	return {
		cap: rows.map((row) => row.cap).join("/"),
		label: label ?? rows[0].label,
	};
}
