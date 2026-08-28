// Native wheel/touch gestures establish reader intent before the browser scrolls.
// A gesture that is clamped cannot scroll, though, and must leave live painting
// enabled instead of latching the renderer in reading mode.

import { afterEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import type { MouseTracking } from "@roost/shared/cell";
import type { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import type { TerminalMouseForwarding } from "../src/lib/terminalMouseForwarding.ts";

// Bun resolves solid-js to its SSR build, where effects do not run. Use the real
// client implementation so bindWheelAndTouchMove owns listeners as it does in
// the browser.
// @ts-expect-error solid-js does not publish types for its dist entry
const S = await import("solid-js/dist/solid.js") as unknown as typeof SolidApi;
mock.module("solid-js", () => ({ ...S }));
const { attachTerminalMouseForwarding } = await import(
	"../src/lib/terminalMouseForwarding.ts"
);

type Listener = (event: unknown) => void;

class FakeDisplay {
	scrollTop = 0;
	scrollHeight = 100;
	clientHeight = 100;
	private readonly listeners = new Map<string, Set<Listener>>();

	addEventListener(type: string, listener: Listener): void {
		let listeners = this.listeners.get(type);
		if (!listeners) {
			listeners = new Set();
			this.listeners.set(type, listeners);
		}
		listeners.add(listener);
	}

	removeEventListener(type: string, listener: Listener): void {
		this.listeners.get(type)?.delete(listener);
	}

	dispatch(type: string, event: unknown): void {
		for (const listener of this.listeners.get(type) ?? []) listener(event);
	}
}

interface WheelStub {
	defaultPrevented: boolean;
	deltaY: number;
	clientX: number;
	clientY: number;
	shiftKey: boolean;
	altKey: boolean;
	ctrlKey: boolean;
	metaKey: boolean;
	preventDefault(): void;
}

interface TouchStub {
	defaultPrevented: boolean;
	touches: Array<{ clientX: number; clientY: number }>;
	preventDefault(): void;
}

function wheelEvent(deltaY: number, shiftKey = false): WheelStub {
	const event: WheelStub = {
		defaultPrevented: false,
		deltaY,
		clientX: 4,
		clientY: 4,
		shiftKey,
		altKey: false,
		ctrlKey: false,
		metaKey: false,
		preventDefault: () => { event.defaultPrevented = true; },
	};
	return event;
}

function touchEvent(clientY: number): TouchStub {
	const event: TouchStub = {
		defaultPrevented: false,
		touches: [{ clientX: 4, clientY }],
		preventDefault: () => { event.defaultPrevented = true; },
	};
	return event;
}

interface Harness {
	display: FakeDisplay;
	readerReasons: string[];
	sent: Uint8Array[];
	fireWheel(deltaY: number, shiftKey?: boolean): WheelStub;
	fireTouch(startY: number, endY: number): TouchStub;
}

const cleanups: Array<() => void> = [];
afterEach(() => {
	for (const cleanup of cleanups.splice(0).reverse()) cleanup();
});

function makeHarness(tracking = 0): Harness {
	const display = new FakeDisplay();
	const readerReasons: string[] = [];
	const sent: Uint8Array[] = [];
	const renderer = {
		enterReading: (reason: string) => { readerReasons.push(reason); },
		viewportCellGeometry: () => ({
			left: 0,
			top: 0,
			cellWidth: 10,
			rowHeight: 10,
			cols: 80,
			rows: 24,
		}),
		predictionHost: display as unknown as HTMLElement,
	} as unknown as CellGridRenderer;

	let disposeRoot = () => {};
	const forwarding = S.createRoot((dispose) => {
		disposeRoot = dispose;
		const attached = attachTerminalMouseForwarding({
			display: display as unknown as HTMLDivElement,
			mouseTracking: () => tracking as MouseTracking,
			sendBytes: (bytes) => { sent.push(bytes); },
			getRenderer: () => renderer,
			getMouseSgr: () => true,
			getCellW: () => 10,
			getCellH: () => 10,
			measureCell: () => true,
		});
		attached.bindWheelAndTouchMove();
		return attached;
	}) as TerminalMouseForwarding;
	cleanups.push(() => {
		forwarding.dispose();
		disposeRoot();
	});

	return {
		display,
		readerReasons,
		sent,
		fireWheel(deltaY: number, shiftKey = false): WheelStub {
			const event = wheelEvent(deltaY, shiftKey);
			display.dispatch("wheel", event);
			return event;
		},
		fireTouch(startY: number, endY: number): TouchStub {
			display.dispatch("touchstart", touchEvent(startY));
			const move = touchEvent(endY);
			display.dispatch("touchmove", move);
			display.dispatch("touchend", {});
			return move;
		},
	};
}

describe("terminal mouse native reader intent", () => {
	test("wheel gestures with no overflow leave live painting enabled", () => {
		const h = makeHarness();
		h.fireWheel(-20);
		h.fireWheel(20);

		expect(h.readerReasons).toEqual([]);
	});

	test("wheel intent follows the available direction at both clamped edges", () => {
		const h = makeHarness();
		h.display.scrollHeight = 300;
		h.display.clientHeight = 100;

		h.display.scrollTop = 0;
		h.fireWheel(-20); // wheel up is clamped at the top
		expect(h.readerReasons).toEqual([]);
		h.fireWheel(20); // wheel down can move toward the live tail
		expect(h.readerReasons).toEqual(["wheel"]);

		h.readerReasons.length = 0;
		h.display.scrollTop = 200;
		h.fireWheel(20); // wheel down is clamped at the bottom
		expect(h.readerReasons).toEqual([]);
		h.fireWheel(-20); // wheel up can move into history
		expect(h.readerReasons).toEqual(["wheel"]);
	});

	test("touch gestures with no overflow leave live painting enabled", () => {
		const h = makeHarness();
		h.fireTouch(100, 120); // finger down would scroll toward history
		h.fireTouch(100, 80); // finger up would scroll toward the live tail

		expect(h.readerReasons).toEqual([]);
	});

	test("touch intent follows the available direction at both clamped edges", () => {
		const h = makeHarness();
		h.display.scrollHeight = 300;
		h.display.clientHeight = 100;

		h.display.scrollTop = 0;
		h.fireTouch(100, 120); // finger down is clamped at the top
		expect(h.readerReasons).toEqual([]);
		h.fireTouch(100, 80); // finger up can move toward the live tail
		expect(h.readerReasons).toEqual(["touch"]);

		h.readerReasons.length = 0;
		h.display.scrollTop = 200;
		h.fireTouch(100, 80); // finger up is clamped at the bottom
		expect(h.readerReasons).toEqual([]);
		h.fireTouch(100, 120); // finger down can move into history
		expect(h.readerReasons).toEqual(["touch"]);
	});

	test("application forwarding still owns clamped wheel and touch gestures", () => {
		const h = makeHarness(1002);
		const wheel = h.fireWheel(20);
		const touch = h.fireTouch(100, 90);

		expect(wheel.defaultPrevented).toBe(true);
		expect(touch.defaultPrevented).toBe(true);
		expect(h.sent).toHaveLength(2);
		expect(h.readerReasons).toEqual([]);
	});

	test("a native wheel bypass is guarded by the same scroll feasibility", () => {
		const h = makeHarness(1002);
		h.display.scrollHeight = 300;
		h.display.clientHeight = 100;

		const clamped = h.fireWheel(-20, true);
		expect(clamped.defaultPrevented).toBe(false);
		expect(h.readerReasons).toEqual([]);

		h.display.scrollTop = 50;
		const scrollable = h.fireWheel(-20, true);
		expect(scrollable.defaultPrevented).toBe(false);
		expect(h.readerReasons).toEqual(["wheel"]);
	});
});
