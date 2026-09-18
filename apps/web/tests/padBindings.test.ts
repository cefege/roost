// Pins the controller contract a keyboard-free session depends on: the stick
// clicks are bound at all (they are the only path to the mic and to another
// folder), no discrete command auto-repeats, and the transient legend can never
// name a control the controller map does not describe.
//
// Data only — importing PAD_HINTS also proves every legend cap resolves, since
// the derivation throws on an unknown one.

import { describe, expect, test } from "bun:test";
import {
	PAD_BUTTON_ACTIONS,
	PAD_CONTROL_GUIDE,
	PAD_HINTS,
	PAD_REPEATING_ACTIONS,
	type PadAction,
	type PadHintContext,
} from "../src/lib/padBindings.ts";

const CONTEXTS: readonly PadHintContext[] = [
	"default", "menu", "overlay", "terminal", "keypad", "dictation",
];

function guideLabel(cap: string): string {
	const row = PAD_CONTROL_GUIDE.find((candidate) => candidate.cap === cap);
	if (!row) throw new Error(`no guide row for ${cap}`);
	return row.label;
}

describe("PAD_BUTTON_ACTIONS", () => {
	test("binds the stick clicks and retires the shortcut-list overlay", () => {
		expect(PAD_BUTTON_ACTIONS[9]).toBe("controller-map");
		expect(PAD_BUTTON_ACTIONS[10]).toBe("mic-toggle");
		expect(PAD_BUTTON_ACTIONS[11]).toBe("folder-next");
		expect(Object.values(PAD_BUTTON_ACTIONS) as string[]).not.toContain("help");
	});

	test("no discrete command auto-repeats while held", () => {
		// A held stick click that fired every frame would restart dictation or
		// walk every folder in a second.
		const discrete: readonly PadAction[] = [
			"mic-toggle", "folder-next", "controller-map", "keypad", "activate", "back",
		];
		for (const action of discrete)
			expect(PAD_REPEATING_ACTIONS[action]).toBeUndefined();
	});
});

describe("PAD_CONTROL_GUIDE", () => {
	test("describes every bound button and every legend cap", () => {
		// The D-pad and stick rows cover a direction family, so they carry no
		// single action; every other binding must name its own row.
		const clustered: readonly PadAction[] = [
			"move-up", "move-down", "move-left", "move-right",
		];
		const described = new Set(PAD_CONTROL_GUIDE.map((row) => row.action));
		const undocumented = Object.values(PAD_BUTTON_ACTIONS).filter(
			(action) => !described.has(action) && !clustered.includes(action),
		);
		expect(undocumented).toEqual([]);

		const caps = new Set(PAD_CONTROL_GUIDE.map((row) => row.cap));
		const unknownCaps = CONTEXTS.flatMap((context) => {
			expect(PAD_HINTS[context].length).toBeGreaterThan(0);
			return PAD_HINTS[context].flatMap((hint) => hint.cap.split("/"));
		}).filter((cap) => !caps.has(cap));
		expect(unknownCaps).toEqual([]);
	});

	test("a legend row that does not override its verb reuses the guide's", () => {
		const mic = PAD_HINTS.default.find((hint) => hint.cap === "L3");
		expect(mic?.label).toBe(guideLabel("L3"));
		const tab = PAD_HINTS.terminal.find((hint) => hint.cap === "LB/RB");
		expect(tab?.label).toBe(guideLabel("LB"));
	});
});

describe("PAD_HINTS", () => {
	test("the key pad and dictation surfaces name their own buttons", () => {
		expect(PAD_HINTS.keypad).toEqual([
			{ cap: "D-pad", label: "Move" },
			{ cap: "A", label: "Press" },
			{ cap: "B", label: "Close" },
		]);
		expect(PAD_HINTS.dictation).toEqual([
			{ cap: "A", label: "Send" },
			{ cap: "B", label: "Discard" },
			{ cap: "L3", label: "Stop" },
		]);
	});

	test("mic, folders, and the guide are advertised where they work", () => {
		for (const context of ["default", "terminal"] as const) {
			expect(PAD_HINTS[context]).toContainEqual({ cap: "L3", label: "Mic" });
			expect(PAD_HINTS[context]).toContainEqual({ cap: "R3", label: "Folder" });
			expect(PAD_HINTS[context]).toContainEqual({ cap: "Start", label: "Guide" });
		}
	});
});
