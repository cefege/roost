// attachTerminalLinks visibility-recovery tripwire.
//
// The scan latch (`scanScheduled`) has exactly one reset path: `scan` running
// (terminal-links.ts), which only happens via the rAF queued in `scheduleScan`.
// If the browser DROPS that queued rAF during a long-backgrounded / throttled /
// slept tab — not merely defers it — the latch sticks `true` forever, every
// later `scheduleScan()` no-ops, and the cell renderer's per-frame row rebuilds
// destroy <a> anchors with no re-linkify → Cmd arms but there's nothing to
// click → "dead" until a refresh. The fix: a `visibilitychange` listener that
// cancels any stale/deferred frame, resets the latch, and forces a full scan.
//
// No jsdom (by design — see cellRenderer.dom.test.ts). A typed fake DOM covers
// exactly the recovery path. Rows carry no linkable text so `_linkifyRows`
// early-returns (segments.length===0) and the Range/TreeWalker API is untouched.

import { describe, test, expect, afterEach } from "bun:test";
import { attachTerminalLinks } from "../src/components/terminal-links.ts";

// ── minimal fake DOM ──────────────────────────────────────────────────────
class FakeEl {
	tagName: string;
	ownerDocument: unknown;
	textContent = "";
	childNodes: unknown[] = [];
	replacedWith: unknown[] | null = null;
	private attrs = new Map<string, string>();
	private listeners = new Map<string, Set<(ev: unknown) => void>>();
	children: FakeEl[] = [];
	parentElement: FakeEl | null = null;
	style = { getPropertyValue: (_k: string): string => "" };
	classList = { add: () => {}, remove: () => {}, contains: () => false, toggle: () => false };
	// Mutable so a test can spy on the full-scan querySelectorAll call.
	querySelectorAll: (_sel: string) => FakeEl[];
	constructor(tag: string, doc: unknown) {
		this.tagName = tag;
		this.ownerDocument = doc;
		this.querySelectorAll = () => [];
	}
	appendChild(c: FakeEl): FakeEl { c.parentElement = this; this.children.push(c); return c; }
	setAttribute(k: string, v: string): void { this.attrs.set(k, v); }
	getAttribute(k: string): string | null { return this.attrs.get(k) ?? null; }
	removeAttribute(k: string): void { this.attrs.delete(k); }
	querySelector(_sel: string): FakeEl | null { return null; }
	addEventListener(type: string, fn: (ev: unknown) => void): void {
		let listeners = this.listeners.get(type);
		if (!listeners) { listeners = new Set(); this.listeners.set(type, listeners); }
		listeners.add(fn);
	}
	removeEventListener(type: string, fn: (ev: unknown) => void): void { this.listeners.get(type)?.delete(fn); }
	dispatchEvent(ev: { type: string; [key: string]: unknown }): void {
		for (const fn of this.listeners.get(ev.type) ?? []) fn(ev);
	}
	remove(): void {}
	replaceWith(...nodes: unknown[]): void { this.replacedWith = nodes; }
}

class FakeEventTarget {
	private listeners = new Map<string, Set<(ev: unknown) => void>>();
	addEventListener(type: string, fn: (ev: unknown) => void): void {
		let s = this.listeners.get(type);
		if (!s) { s = new Set(); this.listeners.set(type, s); }
		s.add(fn);
	}
	removeEventListener(type: string, fn: (ev: unknown) => void): void {
		this.listeners.get(type)?.delete(fn);
	}
	dispatchEvent(ev: { type: string; [key: string]: unknown }): void {
		for (const fn of this.listeners.get(ev.type) ?? []) fn(ev);
	}
}

class FakeDoc extends FakeEventTarget {
	visibilityState = "visible";
	readonly head: FakeEl;
	constructor() { super(); this.head = new FakeEl("head", this); }
	createElement(tag: string): FakeEl { return new FakeEl(tag, this); }
}

class FakeMutationObserver {
	constructor(public cb: (muts: unknown[]) => void) {}
	observe(): void {}
	disconnect(): void {}
}

interface RafEntry { handle: number; cb: () => void }

interface Harness {
	doc: FakeDoc;
	container: FakeEl;
	win: FakeEventTarget;
	rafQueue: RafEntry[];
	fireNextRaf: () => void;
	fireAllRaf: () => void;
	restore: () => void;
}

function makeHarness(): Harness {
	const doc = new FakeDoc();
	const container = new FakeEl("div", doc);
	const win = new FakeEventTarget();
	const nav = { userAgent: "Macintosh", platform: "MacIntel", userAgentData: undefined };
	const rafQueue: RafEntry[] = [];
	let nextHandle = 1;
	const raf = (cb: () => void): number => {
		const handle = nextHandle++;
		rafQueue.push({ handle, cb });
		return handle;
	};
	// Real splice (not a no-op): a dropped/deferred rAF must be removable so the
	// fix's cancelAnimationFrame path is actually exercised.
	const cancelRaf = (handle: number): void => {
		const i = rafQueue.findIndex((e) => e.handle === handle);
		if (i >= 0) rafQueue.splice(i, 1);
	};
	const fireNextRaf = (): void => { rafQueue.shift()?.cb(); };
	const fireAllRaf = (): void => {
		const entries = rafQueue.splice(0);
		for (const e of entries) e.cb();
	};

	// Install on globalThis — attachTerminalLinks reads document/window/navigator/
	// MutationObserver/requestAnimationFrame as bare globals. Cast: globalThis is
	// writable at runtime but TS declares these readonly-ish; the fake is
	// structurally equivalent for the recovery path (unexpressible DOM type).
	const g = globalThis as unknown as Record<string, unknown>;
	const saved: Record<string, unknown> = {
		document: g.document, window: g.window, navigator: g.navigator,
		MutationObserver: g.MutationObserver,
		requestAnimationFrame: g.requestAnimationFrame,
		cancelAnimationFrame: g.cancelAnimationFrame,
	};
	g.document = doc;
	g.window = win;
	g.navigator = nav;
	g.MutationObserver = FakeMutationObserver;
	g.requestAnimationFrame = raf;
	g.cancelAnimationFrame = cancelRaf;
	const restore = (): void => { for (const [k, v] of Object.entries(saved)) g[k] = v; };

	return { doc, container, win, rafQueue, fireNextRaf, fireAllRaf, restore };
}

// attachTerminalLinks expects a real HTMLElement; the fake is structurally
// equivalent for the paths exercised here (unchecked cast — DOM type unexpressible).
const asEl = (el: FakeEl): HTMLElement => el as unknown as HTMLElement;

describe("attachTerminalLinks — visibility recovery", () => {
	let h: Harness | undefined;
	afterEach(() => { h?.restore(); h = undefined; });

	test("initial rAF fires → scan runs (happy path, no regression)", () => {
		h = makeHarness();
		const attachment = attachTerminalLinks(asEl(h.container), null, {});
		expect(h.rafQueue.length).toBe(1);
		let scans = 0;
		h.container.querySelectorAll = () => { scans++; return []; };
		h.fireNextRaf();
		expect(scans).toBe(1);
		attachment.dispose();
	});

	test("dropped rAF (stuck latch) → visibilitychange recovers and re-linkifies", () => {
		h = makeHarness();
		const attachment = attachTerminalLinks(asEl(h.container), null, {});
		expect(h.rafQueue.length).toBe(1);
		const staleHandle = h.rafQueue[0]!.handle;

		// Model the bug: the browser DROPS the queued rAF during a long-backgrounded
		// tab (callback removed, never fires) but `scanScheduled` stays true. No
		// later scheduleScan() can queue a new frame — it no-ops while stuck.
		h.rafQueue.length = 0;
		expect(h.rafQueue.length).toBe(0);

		// Tab returns to foreground.
		h.doc.visibilityState = "visible";
		h.doc.dispatchEvent({ type: "visibilitychange" });

		// Recovery re-armed: a fresh rAF is queued (without the fix there is no
		// visibilitychange listener, so this stays 0 and scan never runs).
		expect(h.rafQueue.length).toBe(1);
		expect(h.rafQueue[0]!.handle).not.toBe(staleHandle);

		let scans = 0;
		h.container.querySelectorAll = () => { scans++; return []; };
		h.fireNextRaf();
		expect(scans).toBe(1);
		attachment.dispose();
	});

	test("deferred rAF → visibilitychange cancels the stale frame (no double-scan)", () => {
		h = makeHarness();
		const attachment = attachTerminalLinks(asEl(h.container), null, {});
		expect(h.rafQueue.length).toBe(1);
		const staleHandle = h.rafQueue[0]!.handle;

		// rAF is merely DEFERRED (still queued, not yet fired) — the normal case
		// when the tab was hidden only briefly.
		h.doc.visibilityState = "visible";
		h.doc.dispatchEvent({ type: "visibilitychange" });

		// The stale frame was cancelled and a fresh one queued: exactly one entry,
		// and it is NOT the stale handle. (If cancel were a no-op, two entries
		// would remain and firing both would scan twice.)
		expect(h.rafQueue.length).toBe(1);
		expect(h.rafQueue[0]!.handle).not.toBe(staleHandle);

		let scans = 0;
		h.container.querySelectorAll = () => { scans++; return []; };
		h.fireAllRaf();
		expect(scans).toBe(1);
		attachment.dispose();
	});

	test("late mapping refresh unwraps stale anchors and resets a dropped scan latch", () => {
		h = makeHarness();
		const attachment = attachTerminalLinks(asEl(h.container), null, {});
		const anchor = new FakeEl("a", h.doc);
		const child = new FakeEl("span", h.doc);
		anchor.childNodes = [child];
		let rowScans = 0;
		h.container.querySelectorAll = (selector) => {
			if (selector === "a.wterm-link") return [anchor];
			rowScans += 1;
			return [];
		};

		// Model a browser-dropped initial callback with scanScheduled still true.
		h.rafQueue.length = 0;
		attachment.refresh();

		expect(anchor.replacedWith).toEqual([child]);
		expect(h.rafQueue.length).toBe(1);
		h.fireNextRaf();
		expect(rowScans).toBe(1);
		attachment.dispose();
	});

	test("teardown removes the visibilitychange listener (no leak)", () => {
		h = makeHarness();
		const attachment = attachTerminalLinks(asEl(h.container), null, {});
		attachment.dispose();
		// After teardown, a visibility flip must not schedule any scan.
		h.doc.visibilityState = "visible";
		h.doc.dispatchEvent({ type: "visibilitychange" });
		expect(h.rafQueue.length).toBe(0);
	});

	test("releaseInteraction clears modifier and pointer state and releases a hold once", () => {
		h = makeHarness();
		const changes: boolean[] = [];
		const attachment = attachTerminalLinks(asEl(h.container), null, {
			onArmedHoverChange: (active) => changes.push(active),
		});
		h.container.dispatchEvent({ type: "mouseenter" });
		h.win.dispatchEvent({ type: "keydown", key: "Meta" });
		expect(h.container.getAttribute("data-link-armed")).toBe("1");
		expect(changes).toEqual([true]);

		attachment.releaseInteraction();
		expect(h.container.getAttribute("data-link-armed")).toBeNull();
		expect(changes).toEqual([true, false]);
		attachment.releaseInteraction();
		expect(changes).toEqual([true, false]);

		// Pointer state was cleared: re-arming alone cannot reacquire the hold.
		h.win.dispatchEvent({ type: "keydown", key: "Meta" });
		expect(changes).toEqual([true, false]);
		attachment.dispose();
	});
});
