// Which seam each controller press reaches — the difference between a pad that
// can finish a task and one that dead-ends. A/B must mean COMMIT/DISCARD while
// dictating, mean press-a-key/leave while the terminal key pad is open, and a
// mic press with no grant must explain itself rather than do nothing at all.
// The key-pad seam and the folder cycle have their own suites; this one pins
// the routing. Bun has no browser DOM, so the document is hand-rolled (repo
// convention: no jsdom — see attachmentsPicker.dom.test.ts).

import { beforeEach, describe, expect, mock, test } from "bun:test";
import type { SessionId } from "@roost/protocol/wire";

// ── globals the module touches ──────────────────────────────────────────────

const storage: Record<string, string> = {};
(globalThis as unknown as { localStorage: Storage }).localStorage = {
	getItem: (key: string) => storage[key] ?? null,
	setItem: (key: string, value: string) => {
		storage[key] = value;
	},
	removeItem: (key: string) => {
		delete storage[key];
	},
} as Storage;
(globalThis as unknown as { location: { search: string } }).location = { search: "" };

/** `closest()` answers truthiness only, which is all the dispatcher asks of it,
 *  so an ancestor match reports this element rather than a parent chain. */
class FakeElement {
	focuses = 0;
	clicks = 0;
	keys: string[] = [];

	constructor(
		private readonly selectors: readonly string[],
		private readonly ancestors: readonly string[] = [],
	) {}

	matches(query: string): boolean {
		return this.parts(query).some((part) => this.selectors.includes(part));
	}

	closest(query: string): FakeElement | null {
		const parts = this.parts(query);
		const hit = parts.some(
			(part) => this.selectors.includes(part) || this.ancestors.includes(part),
		);
		return hit ? this : null;
	}

	focus(): void {
		this.focuses++;
		fakeDocument.activeElement = this;
	}

	click(): void {
		this.clicks++;
	}

	dispatchEvent(event: { key?: string }): boolean {
		if (event.key !== undefined) this.keys.push(event.key);
		return true;
	}

	private parts(query: string): string[] {
		return query.split(",").map((part) => part.trim());
	}
}

const wterm = new FakeElement([".wterm"]);
const navKey = new FakeElement(["button"], [".term-nav"]);
const row = new FakeElement(["a"]);

const queryResults: Record<string, FakeElement | null> = { ".wterm": wterm };
const fakeDocument = {
	activeElement: null as FakeElement | null,
	body: new FakeElement(["body"]),
	documentElement: { setAttribute: () => {} },
	querySelector: (selector: string) => queryResults[selector] ?? null,
};
(globalThis as unknown as { document: unknown }).document = fakeDocument;

class FakeKeyboardEvent {
	readonly key: string;
	constructor(readonly type: string, init: { key: string }) {
		this.key = init.key;
	}
}
(globalThis as unknown as { KeyboardEvent: unknown }).KeyboardEvent = FakeKeyboardEvent;

// ── seams ───────────────────────────────────────────────────────────────────

const keypad = { open: false, toggles: 0, closes: 0, focusFirst: 0, cancels: 0 };
mock.module("../src/components/TerminalNavButtons.tsx", () => ({
	terminalNavPadOpen: () => keypad.open,
	toggleTerminalNavPad: () => {
		keypad.toggles++;
		keypad.open = !keypad.open;
	},
	closeTerminalNavPad: () => {
		keypad.closes++;
		keypad.open = false;
	},
	focusTerminalNavPadFirstKey: () => {
		keypad.focusFirst++;
		return () => {
			keypad.cancels++;
		};
	},
}));

let folderGroups: { key: string; leadId: string }[] = [];
mock.module("../src/lib/folderGroups.ts", () => ({
	buildFolderGroups: () => folderGroups,
}));

// Dynamic by necessity: a static import hoists above the mock.module calls.
const { padHintContext, runPadActions, setPadRouterIo } = await import(
	"../src/lib/padActions.ts"
);
const { setPadModeChoice } = await import("../src/lib/padMode.ts");
const { closeControllerMap, cmdPaletteOpen, controllerMapOpen } = await import(
	"../src/lib/keyboardShortcuts.ts"
);
const { registerVoiceControls, setActiveVoiceOwner } = await import(
	"../src/lib/voiceState.ts"
);
const { toasts } = await import("../src/store/toastStore.ts");

const mic = { toggles: 0, discards: 0, startable: true };
const micControls = {
	toggle: () => {
		mic.toggles++;
	},
	discard: () => {
		mic.discards++;
	},
	canStartWithoutGesture: () => mic.startable,
};

function dictating(active: boolean): void {
	setActiveVoiceOwner(active ? { sessionId: "s1" as SessionId, token: 1 } : null);
}

setPadModeChoice("on");

beforeEach(() => {
	keypad.open = false;
	keypad.toggles = 0;
	keypad.closes = 0;
	keypad.focusFirst = 0;
	keypad.cancels = 0;
	mic.toggles = 0;
	mic.discards = 0;
	mic.startable = true;
	wterm.focuses = 0;
	wterm.keys = [];
	navKey.keys = [];
	navKey.clicks = 0;
	row.keys = [];
	row.clicks = 0;
	folderGroups = [];
	fakeDocument.activeElement = null;
	dictating(false);
	registerVoiceControls(null);
	setPadRouterIo(null);
	if (controllerMapOpen()) closeControllerMap();
});

describe("terminal key pad routing", () => {
	test("A on the terminal box opens the pad and lands on its first key", () => {
		fakeDocument.activeElement = wterm;

		runPadActions(["activate"]);

		expect(keypad.open).toBe(true);
		expect(keypad.focusFirst).toBe(1);
		// No synthetic key: the box would have swallowed it into the PTY.
		expect(wterm.keys).toEqual([]);
	});

	test("a later open cancels the superseded focus retry", () => {
		fakeDocument.activeElement = wterm;

		runPadActions(["activate"]);
		// The canceller is module state that outlives the press that armed it —
		// the invariant is one live retry, so measure from this open.
		const armed = keypad.cancels;
		runPadActions(["keypad"]); // closes
		runPadActions(["keypad"]); // opens again

		expect(keypad.focusFirst).toBe(2);
		expect(keypad.cancels).toBe(armed + 1);
	});

	test("A on a key presses it instead of re-toggling the pad", () => {
		keypad.open = true;
		fakeDocument.activeElement = navKey;

		runPadActions(["activate"]);

		expect(keypad.toggles).toBe(0);
		expect(navKey.keys).toEqual(["Enter"]);
		expect(navKey.clicks).toBe(1);
	});

	test("B on a key closes the pad and returns focus to the terminal", () => {
		keypad.open = true;
		fakeDocument.activeElement = navKey;

		runPadActions(["back"]);

		expect(keypad.closes).toBe(1);
		expect(wterm.focuses).toBe(1);
		// No Escape: a body-portal key would escape past the pad to the document.
		expect(navKey.keys).toEqual([]);
	});

	test("the legend names the key pad while focus is inside it", () => {
		keypad.open = true;
		fakeDocument.activeElement = navKey;

		runPadActions(["move-down"]);

		expect(padHintContext()).toBe("keypad");
	});
});

describe("dictation routing", () => {
	test("A sends and B discards, and neither reaches the focused element", () => {
		registerVoiceControls(micControls);
		dictating(true);
		fakeDocument.activeElement = row;

		runPadActions(["activate"]);
		expect(mic.toggles).toBe(1);

		runPadActions(["back"]);
		expect(mic.discards).toBe(1);

		expect(row.keys).toEqual([]);
		expect(row.clicks).toBe(0);
		expect(padHintContext()).toBe("dictation");
	});

	test("the stick click starts the mic when the browser would allow it", () => {
		registerVoiceControls(micControls);

		runPadActions(["mic-toggle"]);

		expect(mic.toggles).toBe(1);
	});

	test("a start the browser would deny explains itself exactly once", () => {
		registerVoiceControls(micControls);
		mic.startable = false;
		const before = toasts().length;

		runPadActions(["mic-toggle"]);
		runPadActions(["mic-toggle"]);

		expect(mic.toggles).toBe(0);
		expect(toasts().length - before).toBe(1);
	});

	test("no mounted composer leaves the press inert", () => {
		const before = toasts().length;

		runPadActions(["mic-toggle"]);

		expect(mic.toggles).toBe(0);
		expect(toasts().length).toBe(before);
	});
});

describe("folder and guide routing", () => {
	test("the stick click opens the next folder's newest session", () => {
		const routes: string[] = [];
		setPadRouterIo({ navigate: (href) => routes.push(href), getPath: () => "/" });
		folderGroups = [
			{ key: "web", leadId: "web-new" },
			{ key: "api", leadId: "api-new" },
		];

		runPadActions(["folder-next"]);
		expect(routes).toEqual(["/s/web-new"]);

		// Nothing to switch to must not navigate to a stale target.
		folderGroups = [];
		runPadActions(["folder-next"]);
		expect(routes).toEqual(["/s/web-new"]);
	});

	test("Start toggles the controller map", () => {
		expect(controllerMapOpen()).toBe(false);

		runPadActions(["controller-map"]);
		expect(controllerMapOpen()).toBe(true);

		runPadActions(["controller-map"]);
		expect(controllerMapOpen()).toBe(false);
	});

	test("the open map stands the dispatcher down except Start and B", () => {
		runPadActions(["controller-map"]);
		fakeDocument.activeElement = row;

		// Reading the diagram means holding buttons: firing what they document
		// would close the map, stack a palette on it, or switch tabs behind it.
		runPadActions(["activate", "palette", "tab-next", "move-down", "keypad"]);
		expect(controllerMapOpen()).toBe(true);
		expect(cmdPaletteOpen()).toBe(false);
		expect(keypad.toggles).toBe(0);
		expect(row.keys).toEqual([]);
		expect(row.clicks).toBe(0);

		// B closes it outright: a dispatched Escape would dismiss the Kobalte
		// dialog and then fall through to close the drawer behind it as well.
		runPadActions(["back"]);
		expect(controllerMapOpen()).toBe(false);
		expect(row.keys).toEqual([]);
	});
});
