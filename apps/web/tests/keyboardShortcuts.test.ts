// Regression tripwire for the "⏎ jumps me to another workspace" boot loop.
//
// Root cause (post pane-tiling): keyboardShortcuts.terminalOwnsKeyboard() keyed
// off `[data-pane][data-focused="true"]` — a per-slot flag absent during every
// layout reconcile + the ResizeObserver 0×0 window. In those transient windows,
// with the wterm textarea off-screen (so isEditableTarget=false), ⏎ fell through
// to activateCursor() → navigate to whatever row the persisted sidebar cursor
// last sat on → teleport to another session/workspace mid-typing. Fixed by
// keying the guard off the STABLE `[data-testid="terminal-deck"]` element + a
// hard ⏎-never-navigates-while-deck-present backstop in the Enter branch.
//
// DOM-free: this repo has no browser-DOM test harness, so we stub the two probes
// handleKeydown actually reads — document.querySelector + a KeyboardEvent shape —
// and assert the sidebar activate handler is never invoked while a deck is up.

import { describe, test, expect, beforeEach, afterEach } from "bun:test";
import {
	handleKeydown,
	terminalOwnsKeyboard,
} from "../src/lib/keyboardShortcuts.ts";
import {
	setActivateHandler,
	setOrderedSessionIds,
	moveCursor,
	cursorSessionId,
} from "../src/lib/sidebarCursor.ts";

// Minimal stand-ins for the DOM surface handleKeydown touches.
type StubDoc = { querySelector: (sel: string) => object | null };
const origDocument = (globalThis as { document?: unknown }).document;

// deckSelectors: which querySelectors should resolve to a (truthy) element.
function installStubDocument(deckPresent: boolean): void {
	const stub: StubDoc = {
		querySelector(sel: string) {
			if (!deckPresent) return null;
			if (sel === '[data-testid="terminal-deck"]' || sel === "[data-pane]")
				return {};
			return null;
		},
	};
	(globalThis as { document?: unknown }).document = stub;
}

// A KeyboardEvent-shaped object — enough for handleKeydown's field reads. target
// defaults to a non-editable element (tagName DIV) = the off-screen-textarea-lost
// -focus case where the bug bites.
function keyEvent(
	key: string,
	target: { tagName?: string; isContentEditable?: boolean } = {
		tagName: "DIV",
	},
): KeyboardEvent {
	return {
		key,
		metaKey: false,
		ctrlKey: false,
		altKey: false,
		shiftKey: false,
		target,
		preventDefault() {
			/* no-op */
		},
	} as unknown as KeyboardEvent;
}

describe("keyboardShortcuts ⏎ boot-loop guard", () => {
	let activated: string[];

	beforeEach(() => {
		activated = [];
		setActivateHandler((id) => activated.push(id));
		// Two sessions in the flat list; park the cursor on the SECOND (the "other
		// workspace" the boot loop teleported to).
		setOrderedSessionIds(["sess-A", "sess-B"]);
		moveCursor(1); // cursor → index 0 (sess-A)
		moveCursor(1); // cursor → index 1 (sess-B)
		expect(cursorSessionId()).toBe("sess-B");
	});

	afterEach(() => {
		setActivateHandler(null);
		setOrderedSessionIds([]);
		(globalThis as { document?: unknown }).document = origDocument;
	});

	test("terminalOwnsKeyboard() true whenever the deck element is present", () => {
		installStubDocument(true);
		expect(terminalOwnsKeyboard()).toBe(true);
	});

	test("terminalOwnsKeyboard() false when no deck/pane on screen", () => {
		installStubDocument(false);
		expect(terminalOwnsKeyboard()).toBe(false);
	});

	test("⏎ does NOT activate the sidebar cursor while a deck is mounted", () => {
		installStubDocument(true); // deck on screen, focus dropped off the textarea
		handleKeydown(keyEvent("Enter"));
		expect(activated).toEqual([]); // no teleport
	});

	test("⏎ DOES activate the cursor when no terminal deck is present (pure sidebar view)", () => {
		installStubDocument(false);
		handleKeydown(keyEvent("Enter"));
		expect(activated).toEqual(["sess-B"]);
	});

	test("⏎ never activates while typing in an input, deck or not", () => {
		installStubDocument(false);
		handleKeydown(keyEvent("Enter", { tagName: "INPUT" }));
		expect(activated).toEqual([]);
	});
});
