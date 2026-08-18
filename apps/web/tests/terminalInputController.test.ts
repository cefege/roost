import { describe, expect, test } from "bun:test";
import { TerminalInputController } from "../src/lib/terminalInputController.ts";

class FakeClassList {
	private readonly values = new Set<string>();
	add(value: string): void { this.values.add(value); }
	remove(value: string): void { this.values.delete(value); }
	contains(value: string): boolean { return this.values.has(value); }
}

type FakeEvent = Pick<Event, "type">;

type FakeKeyEvent = Pick<
	KeyboardEvent,
	"key" | "code" | "ctrlKey" | "altKey" | "shiftKey" | "metaKey" | "isComposing" | "preventDefault"
> & {
	type: "keydown";
	prevented: boolean;
	getModifierState?: KeyboardEvent["getModifierState"];
};

class FakeElement {
	readonly classList = new FakeClassList();
	readonly listeners = new Map<string, Set<(event: FakeEvent) => void>>();
	readonly attributes = new Map<string, string>();
	className = "";
	value = "";
	tabIndex = -1;
	isConnected = false;
	parent: FakeElement | null = null;

	constructor(readonly ownerDocument: FakeDocument) {}
	setAttribute(name: string, value: string): void { this.attributes.set(name, value); }
	appendChild(child: FakeElement): FakeElement {
		child.parent = this;
		child.isConnected = true;
		return child;
	}
	addEventListener(type: string, listener: (event: FakeEvent) => void): void {
		let listeners = this.listeners.get(type);
		if (!listeners) {
			listeners = new Set();
			this.listeners.set(type, listeners);
		}
		listeners.add(listener);
	}
	removeEventListener(type: string, listener: (event: FakeEvent) => void): void {
		this.listeners.get(type)?.delete(listener);
	}
	dispatchEvent(event: FakeEvent): boolean {
		for (const listener of this.listeners.get(event.type) ?? []) listener(event);
		return true;
	}
	focus(): void {
		this.ownerDocument.activeElement = this;
		this.dispatchEvent({ type: "focus" });
	}
	blur(): void {
		this.ownerDocument.activeElement = null;
		this.dispatchEvent({ type: "blur" });
	}
	remove(): void {
		this.isConnected = false;
		this.parent = null;
		if (this.ownerDocument.activeElement === this) this.ownerDocument.activeElement = null;
	}
}

class FakeDocument {
	activeElement: FakeElement | null = null;
	readonly defaultView = {
		FocusEvent: class {
			readonly type: string;
			constructor(type: string, _init?: unknown) { this.type = type; }
		},
	};
	createElement(): FakeElement { return new FakeElement(this); }
	getSelection(): { toString(): string } { return { toString: () => "" }; }
	createRange(): { selectNodeContents(node: unknown): void } {
		return { selectNodeContents: () => {} };
	}
}

function key(keyValue: string, init: Partial<FakeKeyEvent> = {}): FakeKeyEvent {
	const event: FakeKeyEvent = {
		type: "keydown",
		key: keyValue,
		code: "",
		ctrlKey: false,
		altKey: false,
		shiftKey: false,
		metaKey: false,
		isComposing: false,
		prevented: false,
		preventDefault: () => { event.prevented = true; },
	};
	return Object.assign(event, init);
}

function setup(application = false, focusEvents = false): {
	controller: TerminalInputController;
	root: FakeElement;
	data: string[];
	pastes: string[];
} {
	const doc = new FakeDocument();
	const root = new FakeElement(doc);
	root.isConnected = true;
	const data: string[] = [];
	const pastes: string[] = [];
	const controller = new TerminalInputController(root as unknown as HTMLElement, {
		cursorKeysApplication: () => application,
		focusEventsEnabled: () => focusEvents,
		onData: (value) => data.push(value),
		onPaste: (value) => pastes.push(value),
	});
	return { controller, root, data, pastes };
}

function emit<TEvent extends FakeEvent>(controller: TerminalInputController, event: TEvent): void {
	(controller.textarea as unknown as FakeElement).dispatchEvent(event);
}

describe("TerminalInputController", () => {
	test("encodes physical keys synchronously from the pane's application mode", () => {
		const normal = setup(false);
		const normalUp = key("ArrowUp");
		emit(normal.controller, normalUp);
		expect(normalUp.prevented).toBe(true);
		expect(normal.data).toEqual(["\x1b[A"]);

		const application = setup(true);
		emit(application.controller, key("ArrowUp"));
		expect(application.data).toEqual(["\x1bOA"]);
	});

	test("emits AltGraph text without a Ctrl byte or Alt escape prefix", () => {
		const pane = setup();
		emit(pane.controller, key("€", {
			ctrlKey: true,
			altKey: true,
			getModifierState: (name: string) => name === "AltGraph",
		}));
		expect(pane.data).toEqual(["€"]);
	});

	test("commits Chromium and fallback IME sequences exactly once", async () => {
		const pane = setup();
		const textarea = pane.controller.textarea as unknown as FakeElement;

		emit(pane.controller, { type: "compositionstart" });
		emit(pane.controller, key("Process", { isComposing: true }));
		textarea.value = "é";
		emit(pane.controller, { type: "compositionend", data: "é" });
		emit(pane.controller, { type: "input", data: "é", isComposing: false });
		await Promise.resolve();
		expect(pane.data).toEqual(["é"]);

		emit(pane.controller, { type: "compositionstart" });
		textarea.value = "中";
		emit(pane.controller, { type: "compositionend", data: "中" });
		await Promise.resolve();
		expect(pane.data).toEqual(["é", "中"]);
	});

	test("admits one native paste and leaves framing/normalization to the caller", () => {
		const pane = setup();
		const paste = {
			type: "paste",
			prevented: false,
			preventDefault() { this.prevented = true; },
			clipboardData: { getData: () => "one\r\ntwo" },
		};
		emit(pane.controller, paste);
		expect(paste.prevented).toBe(true);
		expect(pane.pastes).toEqual(["one\r\ntwo"]);
		expect(pane.data).toEqual([]);
	});

	test("preserves the blur/focus dance and removes pane-local listeners", () => {
		const pane = setup();
		pane.controller.forceFocus();
		expect(pane.root.classList.contains("focused")).toBe(true);
		expect(pane.controller.ownsTarget(pane.controller.textarea)).toBe(true);
		pane.controller.forceFocus();
		expect(pane.root.classList.contains("focused")).toBe(true);

		pane.controller.destroy();
		expect(pane.root.classList.contains("focused")).toBe(false);
		emit(pane.controller, key("x"));
		expect(pane.data).toEqual([]);
	});

	// DECSET 1004. The application asked which surface owns the keyboard, so only
	// a REAL textarea transition answers it — and only a transition: forceFocus
	// blurs and re-focuses an already-focused textarea, plus dispatches its own
	// focus event, and reporting that as lost-then-gained focus made every pane
	// switch run the app's FocusLost handling.
	test("reports real focus and blur as CSI I / CSI O when the app asked for 1004", () => {
		const pane = setup(false, true);
		const textarea = pane.controller.textarea as unknown as FakeElement;

		pane.controller.forceFocus();
		expect(pane.data).toEqual(["\x1b[I"]);
		pane.controller.forceFocus();
		expect(pane.data).toEqual(["\x1b[I"]);

		textarea.blur();
		expect(pane.data).toEqual(["\x1b[I", "\x1b[O"]);
		textarea.blur();
		expect(pane.data).toEqual(["\x1b[I", "\x1b[O"]);
		textarea.focus();
		expect(pane.data).toEqual(["\x1b[I", "\x1b[O", "\x1b[I"]);
	});

	test("stays silent on focus transitions when the app never asked for 1004", () => {
		const pane = setup();
		const textarea = pane.controller.textarea as unknown as FakeElement;
		pane.controller.forceFocus();
		textarea.blur();
		textarea.focus();
		expect(pane.data).toEqual([]);
		expect(pane.root.classList.contains("focused")).toBe(true);
	});
});
