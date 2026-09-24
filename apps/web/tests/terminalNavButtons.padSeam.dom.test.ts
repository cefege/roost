// The key pad's controller seam: a pad with no pointer must be able to read
// whether the pad is open, close it (which is the ONLY way to drop a latched
// Ctrl), and land focus on the first key once the Portal has mounted.
// Bun has no browser DOM and Solid resolves to its SSR build here, so this
// suite uses the repo's client-Solid virtual renderer (see pairRequestCard
// .dom.test.ts) plus a hand-rolled document/rAF pair for the focus retry.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";

const PAD_OPEN_KEY = "roostNavPadOpen";

// ── globals the module touches ──────────────────────────────────────────────

const storage: Record<string, string> = {};
const storageWrites: string[] = [];
(globalThis as unknown as { localStorage: Storage }).localStorage = {
	getItem: (key: string) => storage[key] ?? null,
	setItem: (key: string, value: string) => { storage[key] = value; storageWrites.push(`${key}=${value}`); },
	removeItem: (key: string) => { delete storage[key]; },
	clear: () => { for (const key of Object.keys(storage)) delete storage[key]; },
	key: () => null,
	length: 0,
} as Storage;

interface FakeKey { focus: () => void; focuses: number }
function makeKey(): FakeKey {
	return {
		focuses: 0,
		focus() { this.focuses++; fakeDocument.activeElement = this; },
	};
}

let gridKey: FakeKey | null = null;
const selectors: string[] = [];
const fakeDocument = {
	activeElement: null as unknown,
	querySelector(selector: string): unknown {
		selectors.push(selector);
		return gridKey;
	},
};
(globalThis as unknown as { document: unknown }).document = fakeDocument;

let nextFrameId = 1;
const pendingFrames = new Map<number, () => void>();
(globalThis as unknown as { requestAnimationFrame: unknown }).requestAnimationFrame = (callback: () => void) => {
	const id = nextFrameId++;
	pendingFrames.set(id, callback);
	return id;
};
(globalThis as unknown as { cancelAnimationFrame: unknown }).cancelAnimationFrame = (id: number) => {
	pendingFrames.delete(id);
};

function pumpFrame(): void {
	const due = [...pendingFrames.values()];
	pendingFrames.clear();
	for (const callback of due) callback();
}

// ── virtual renderer ────────────────────────────────────────────────────────

interface VNode { tag: unknown; props: Record<string, unknown>; rendered?: unknown }

const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => Solid);

function createElement(tag: unknown, props: Record<string, unknown> | null, ...children: unknown[]): VNode {
	const merged = { ...(props ?? {}) };
	if (children.length > 0) merged.children = children.length === 1 ? children[0] : children;
	const vnode: VNode = { tag, props: merged };
	if (typeof tag === "function") {
		const component = tag as (props: Record<string, unknown>) => unknown;
		const owner = Solid.getOwner();
		vnode.rendered = Solid.runWithOwner(owner, () => component(merged));
	}
	return vnode;
}

const ReactShim = { Fragment: Symbol("term-nav-fragment"), createElement };
(globalThis as typeof globalThis & { React: unknown }).React = ReactShim;
mock.module("react/jsx-dev-runtime", () => ({
	Fragment: ReactShim.Fragment,
	jsxDEV: (tag: unknown, props: Record<string, unknown> | null) => createElement(tag, props),
}));

mock.module("solid-js/web", () => ({
	Portal: (props: Record<string, unknown>) => props.children,
}));
mock.module("../src/store/prefs/mouseForwardPref.ts", () => ({
	mouseForwardEnabled: () => false,
	toggleMouseForward: () => {},
}));
mock.module("../src/components/Settings/md/primitives.tsx", () => ({
	Button: (props: Record<string, unknown>) => props.children,
	Icon: () => null,
	IconButton: () => null,
}));

// Dynamic by necessity: a static import hoists above the mock.module calls the
// module's own imports (Portal, the M3 primitives) need in place first.
const {
	closeTerminalNavPad,
	focusTerminalNavPadFirstKey,
	terminalNavPadOpen,
	toggleTerminalNavPad,
} = await import("../src/store/terminalNavPad.ts");
const { TerminalNavButtons } = await import("../src/components/terminal/TerminalNavButtons.tsx");

/** Mount one sheet so its disarm registration is live, as a focused pane does. */
function mountSheet(): { ctrl: number; link: number; dispose: () => void } {
	const counts = { ctrl: 0, link: 0, dispose: () => {} };
	counts.dispose = Solid.createRoot((dispose) => {
		TerminalNavButtons({
			onKey: () => {},
			ctrlArmed: true,
			onCtrlArmedChange: () => { counts.ctrl++; },
			linkActivationArmed: true,
			onLinkActivationArmedChange: () => { counts.link++; },
		});
		return dispose;
	});
	return counts;
}

beforeEach(() => {
	if (terminalNavPadOpen()) closeTerminalNavPad();
	storageWrites.length = 0;
	selectors.length = 0;
	pendingFrames.clear();
	gridKey = null;
	fakeDocument.activeElement = null;
});

describe("terminal nav pad controller seam", () => {
	test("terminalNavPadOpen is a tracked read of the shared toggle", () => {
		let observed!: () => boolean;
		const dispose = Solid.createRoot((disposeRoot) => {
			observed = Solid.createMemo(terminalNavPadOpen);
			return disposeRoot;
		});

		expect(observed()).toBe(false);
		toggleTerminalNavPad();
		// A snapshot instead of an accessor would leave the memo stale here.
		expect(observed()).toBe(true);
		expect(terminalNavPadOpen()).toBe(true);
		expect(storage[PAD_OPEN_KEY]).toBe("1");

		toggleTerminalNavPad();
		expect(observed()).toBe(false);
		expect(storage[PAD_OPEN_KEY]).toBe("0");
		dispose();
	});

	test("closeTerminalNavPad no-ops when closed and disarms a real close", () => {
		const sheet = mountSheet();

		closeTerminalNavPad();
		expect(sheet.ctrl).toBe(0);
		expect(sheet.link).toBe(0);
		expect(storageWrites).toEqual([]);

		toggleTerminalNavPad();
		expect(terminalNavPadOpen()).toBe(true);
		// Opening must not run the close path's disarm.
		expect(sheet.ctrl).toBe(0);

		closeTerminalNavPad();
		expect(terminalNavPadOpen()).toBe(false);
		expect(sheet.ctrl).toBe(1);
		expect(sheet.link).toBe(1);
		sheet.dispose();
	});

	test("toggleTerminalNavPad closing runs the same disarm path", () => {
		const sheet = mountSheet();
		toggleTerminalNavPad();
		toggleTerminalNavPad();

		expect(terminalNavPadOpen()).toBe(false);
		expect(sheet.ctrl).toBe(1);
		expect(sheet.link).toBe(1);
		sheet.dispose();
	});

	test("focusTerminalNavPadFirstKey focuses the first grid key once it mounts", async () => {
		focusTerminalNavPadFirstKey();
		await Promise.resolve();

		expect(selectors[0]).toContain(".term-nav__grid");
		expect(pendingFrames.size).toBe(1);

		const key = makeKey();
		gridKey = key;
		pumpFrame();

		expect(key.focuses).toBe(1);
		expect(fakeDocument.activeElement).toBe(key);
		// Focus took: the retry must stop instead of re-focusing every frame.
		expect(pendingFrames.size).toBe(0);
	});

	test("focusTerminalNavPadFirstKey retry is bounded and cancellable", async () => {
		const cancel = focusTerminalNavPadFirstKey();
		await Promise.resolve();
		expect(pendingFrames.size).toBe(1);

		cancel();
		expect(pendingFrames.size).toBe(0);
		gridKey = makeKey();
		pumpFrame();
		expect(gridKey.focuses).toBe(0);

		focusTerminalNavPadFirstKey();
		gridKey = null;
		await Promise.resolve();
		for (let frame = 0; frame < 8; frame++) pumpFrame();
		expect(pendingFrames.size).toBe(0);
	});
});
