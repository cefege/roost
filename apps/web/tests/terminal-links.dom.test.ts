// attachTerminalLinks visibility-recovery tripwire.
// The scan latch (`scanScheduled`) resets only when a scheduled scan runs or
// the attachment explicitly cancels it. If a browser drops that rAF while the
// tab is hidden, later mutations cannot queue work and rebuilt anchors remain
// unlinked. Visibility recovery cancels stale work and queues one bounded
// hot-tail pass instead of revisiting retained history.
//
// No jsdom (by design — see cellRenderer.dom.test.ts). A typed fake DOM covers
// exactly the recovery path. Rows carry no linkable text so `_linkifyRows`
// early-returns (segments.length===0) and the Range/TreeWalker API is untouched.

import { describe, test, expect, afterEach } from "bun:test";
import { attachTerminalLinks, isTerminalLinkActivationGesture } from "../src/renderer/terminal-links.ts";

// ── minimal fake DOM ──────────────────────────────────────────────────────
class FakeEl {
	tagName: string;
	ownerDocument: unknown;
	className = "";
	textContent = "";
	childNodes: unknown[] = [];
	replacedWith: unknown[] | null = null;
	private attrs = new Map<string, string>();
	private listeners = new Map<string, Set<(ev: unknown) => void>>();
	children: FakeEl[] = [];
	parentElement: FakeEl | null = null;
	style = { getPropertyValue: (_k: string): string => "" };
	classList = { add: () => {}, remove: () => {}, contains: () => false, toggle: () => false };
	querySelector: (_sel: string) => FakeEl | null = () => null;
	querySelectorAll: (_sel: string) => FakeEl[] = () => [];
	constructor(tag: string, doc: unknown) {
		this.tagName = tag;
		this.ownerDocument = doc;
	}
	appendChild(c: FakeEl): FakeEl { c.parentElement = this; this.children.push(c); return c; }
	setAttribute(k: string, v: string): void { this.attrs.set(k, v); }
	getAttribute(k: string): string | null { return this.attrs.get(k) ?? null; }
	hasAttribute(k: string): boolean { return this.attrs.has(k); }
	removeAttribute(k: string): void { this.attrs.delete(k); }
	closest(selector: string): FakeEl | null {
		if (selector.startsWith("a.") && this.tagName === "a" && this.className === "wterm-link") return this;
		return this.parentElement?.closest(selector) ?? null;
	}
	addEventListener(type: string, fn: (ev: unknown) => void): void {
		let listeners = this.listeners.get(type);
		if (!listeners) { listeners = new Set(); this.listeners.set(type, listeners); }
		listeners.add(fn);
	}
	removeEventListener(type: string, fn: (ev: unknown) => void): void { this.listeners.get(type)?.delete(fn); }
	dispatchEvent(ev: { type: string; [key: string]: unknown }): void {
		for (const fn of this.listeners.get(ev.type) ?? []) fn(ev);
	}
	listenerCount(type: string): number { return this.listeners.get(type)?.size ?? 0; }
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
	listenerCount(type: string): number { return this.listeners.get(type)?.size ?? 0; }
}

class FakeDoc extends FakeEventTarget {
	visibilityState = "visible";
	readonly head: FakeEl;
	constructor() { super(); this.head = new FakeEl("head", this); }
	createElement(tag: string): FakeEl { return new FakeEl(tag, this); }
}

class FakeMutationObserver {
	static instances: FakeMutationObserver[] = [];
	observeCalls = 0;
	disconnectCalls = 0;
	constructor(public cb: (muts: unknown[]) => void) {
		FakeMutationObserver.instances.push(this);
	}
	observe(): void { this.observeCalls += 1; }
	disconnect(): void { this.disconnectCalls += 1; }
}

interface RafEntry { handle: number; cb: () => void }

interface Harness {
	doc: FakeDoc;
	container: FakeEl;
	win: FakeEventTarget;
	nav: { userAgent: string; platform: string; userAgentData: undefined };
	rafQueue: RafEntry[];
	fireNextRaf: () => void;
	fireAllRaf: () => void;
	restore: () => void;
}

function makeHarness(): Harness {
	FakeMutationObserver.instances = [];
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

	return { doc, container, win, nav, rafQueue, fireNextRaf, fireAllRaf, restore };
}

// attachTerminalLinks expects a real HTMLElement; the fake is structurally
// equivalent for the paths exercised here (unchecked cast — DOM type unexpressible).
const asEl = (el: FakeEl): HTMLElement => el as unknown as HTMLElement;
const inactiveLinkActivation = () => false;

function clickEvent(target: FakeEl, fields: Partial<MouseEvent> = {}) {
	const event = {
		type: "click",
		target,
		button: fields.button ?? 0,
		metaKey: fields.metaKey ?? false,
		ctrlKey: fields.ctrlKey ?? false,
		shiftKey: fields.shiftKey ?? false,
		altKey: fields.altKey ?? false,
		defaultPrevented: false,
		preventDefault() { event.defaultPrevented = true; },
	};
	return event;
}
describe("attachTerminalLinks — visibility recovery", () => {
	let h: Harness | undefined;
	afterEach(() => { h?.restore(); h = undefined; });

	test("initial activation waits for paint, then scans only the current tail", () => {
		h = makeHarness();
		const attachment = attachTerminalLinks(asEl(h.container), { linkActivationArmed: inactiveLinkActivation });
		expect(h.rafQueue.length).toBe(1);
		let retainedHistoryScans = 0;
		h.container.querySelectorAll = () => {
			retainedHistoryScans += 1;
			return [];
		};
		let scans = 0;
		const viewport = new FakeEl("div", h.doc);
		viewport.querySelectorAll = () => {
			scans += 1;
			return [];
		};
		h.container.querySelector = (selector: string) =>
			selector === ".cell-viewport" ? viewport : null;
		FakeMutationObserver.instances[0]?.cb([]);
		expect(h.rafQueue).toHaveLength(1);
		h.fireNextRaf();
		expect(scans).toBe(0);
		expect(h.rafQueue).toHaveLength(1);
		h.fireNextRaf();
		expect(scans).toBe(1);
		expect(retainedHistoryScans).toBe(0);
		attachment.dispose();
	});
	test("dropped rAF (stuck latch) → visibilitychange recovers and re-linkifies", () => {
		h = makeHarness();
		const attachment = attachTerminalLinks(asEl(h.container), { linkActivationArmed: inactiveLinkActivation });
		expect(h.rafQueue.length).toBe(1);
		const staleHandle = h.rafQueue[0]!.handle;
		// Model a browser dropping the post-paint activation rAF. The stale handle
		// remains armed, so visibility recovery must cancel it before it can queue
		// a replacement current-tail scan.
		h.rafQueue.length = 0;
		expect(h.rafQueue.length).toBe(0);
		// Tab returns to foreground.
		h.doc.visibilityState = "visible";
		h.doc.dispatchEvent({ type: "visibilitychange" });
		// Recovery re-armed: a fresh rAF is queued for the current tail.
		// Without the visibility listener this remains 0 and links stay stale.
		expect(h.rafQueue.length).toBe(1);
		expect(h.rafQueue[0]!.handle).not.toBe(staleHandle);
		let scans = 0;
		const viewport = new FakeEl("div", h.doc);
		viewport.querySelectorAll = () => {
			scans += 1;
			return [];
		};
		h.container.querySelector = (selector: string) =>
			selector === ".cell-viewport" ? viewport : null;
		h.fireNextRaf();
		expect(scans).toBe(1);
		attachment.dispose();
	});
	test("deferred rAF → visibilitychange cancels the stale frame (no double-scan)", () => {
		h = makeHarness();
		const attachment = attachTerminalLinks(asEl(h.container), { linkActivationArmed: inactiveLinkActivation });
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
		const viewport = new FakeEl("div", h.doc);
		viewport.querySelectorAll = () => {
			scans += 1;
			return [];
		};
		h.container.querySelector = (selector: string) =>
			selector === ".cell-viewport" ? viewport : null;
		h.fireAllRaf();
		expect(scans).toBe(1);
		attachment.dispose();
	});
	test("teardown removes the visibilitychange listener (no leak)", () => {
		h = makeHarness();
		const attachment = attachTerminalLinks(asEl(h.container), { linkActivationArmed: inactiveLinkActivation });
		attachment.dispose();
		// After teardown, a visibility flip must not schedule any scan.
		h.doc.visibilityState = "visible";
		h.doc.dispatchEvent({ type: "visibilitychange" });
		expect(h.rafQueue.length).toBe(0);
	});
	test("inactive panes release scanner and modifier work, then restore producer link activation", () => {
		h = makeHarness();
		const opened: string[] = [];
		const holds: boolean[] = [];
		const attachment = attachTerminalLinks(asEl(h.container), {
			linkActivationArmed: inactiveLinkActivation,
			resolveFile: (path, line) => `/file/W/${path}${line ? `#L${line}` : ""}`,
			onOpenFile: (href) => opened.push(href),
			onArmedHoverChange: (active) => holds.push(active),
		});
		const file = new FakeEl("a", h.doc);
		file.className = "wterm-link";
		file.setAttribute("data-terminal-target", "s/f.ts:9");
		file.setAttribute("href", "/file/W/s/f.ts#L9");
		h.container.appendChild(file);
		let retainedHistoryScans = 0;
		h.container.querySelectorAll = () => {
			retainedHistoryScans += 1;
			return [];
		};
		let scans = 0;
		const viewport = new FakeEl("div", h.doc);
		viewport.querySelectorAll = () => {
			scans += 1;
			return [];
		};
		h.container.querySelector = (selector: string) =>
			selector === ".cell-viewport" ? viewport : null;
		h.container.dispatchEvent({ type: "mouseenter" });
		h.win.dispatchEvent({ type: "keydown", key: "Meta" });
		expect(holds).toEqual([true]);
		attachment.setActive(false);
		expect(h.rafQueue).toHaveLength(0);
		expect(FakeMutationObserver.instances[0]?.disconnectCalls).toBe(1);
		expect(h.doc.listenerCount("visibilitychange")).toBe(0);
		expect(h.win.listenerCount("keydown")).toBe(0);
		expect(h.container.listenerCount("click")).toBe(0);
		expect(h.container.getAttribute("data-link-armed")).toBeNull();
		expect(holds).toEqual([true, false]);
		attachment.setActive(true);
		expect(FakeMutationObserver.instances[0]?.observeCalls).toBe(2);
		expect(h.doc.listenerCount("visibilitychange")).toBe(1);
		expect(h.win.listenerCount("keydown")).toBe(1);
		expect(h.container.listenerCount("click")).toBe(1);
		expect(h.rafQueue).toHaveLength(1);
		h.fireNextRaf();
		expect(scans).toBe(0);
		expect(h.rafQueue).toHaveLength(1);
		h.fireNextRaf();
		expect(scans).toBe(1);
		expect(retainedHistoryScans).toBe(0);
		expect(file.parentElement).toBe(h.container);
		const event = clickEvent(file, { metaKey: true });
		h.container.dispatchEvent(event);
		expect(event.defaultPrevented).toBe(true);
		expect(opened).toEqual(["/file/W/s/f.ts#L9"]);
		attachment.dispose();
	});
	test("releaseInteraction clears modifier and pointer state and releases a hold once", () => {
		h = makeHarness();
		const changes: boolean[] = [];
		const attachment = attachTerminalLinks(asEl(h.container), {
			linkActivationArmed: inactiveLinkActivation,
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
	test("local arming persists across link taps while physical Meta remains supported", () => {
		h = makeHarness();
		const opened: string[] = [];
		let linkActivationArmed = false;
		const attachment = attachTerminalLinks(asEl(h.container), {
			linkActivationArmed: () => linkActivationArmed,
			resolveFile: (path, line) => `/file/W/${path}${line ? `#L${line}` : ""}`,
			onOpenFile: (href) => opened.push(href),
		});
		const file = new FakeEl("a", h.doc);
		file.className = "wterm-link";
		file.setAttribute("data-terminal-target", "s/f.ts:9");
		file.setAttribute("href", "/file/W/s/f.ts#L9");
		h.container.appendChild(file);
		const bare = clickEvent(file);
		h.container.dispatchEvent(bare);
		expect(bare.defaultPrevented).toBe(true);
		expect(opened).toEqual([]);
		linkActivationArmed = true;
		for (const event of [clickEvent(file), clickEvent(file)]) {
			h.container.dispatchEvent(event);
			expect(event.defaultPrevented).toBe(true);
		}
		expect(opened).toEqual(["/file/W/s/f.ts#L9", "/file/W/s/f.ts#L9"]);
		linkActivationArmed = false;
		const disarmed = clickEvent(file);
		h.container.dispatchEvent(disarmed);
		expect(disarmed.defaultPrevented).toBe(true);
		expect(opened).toHaveLength(2);
		const meta = clickEvent(file, { metaKey: true });
		h.container.dispatchEvent(meta);
		expect(meta.defaultPrevented).toBe(true);
		expect(opened).toHaveLength(3);
		const custom = new FakeEl("a", h.doc);
		custom.className = "wterm-link";
		custom.setAttribute("data-terminal-target", "vscode://file/s/f.ts");
		custom.setAttribute("href", "vscode://file/s/f.ts");
		h.container.appendChild(custom);
		const unsafe = clickEvent(custom, { metaKey: true });
		h.container.dispatchEvent(unsafe);
		expect(unsafe.defaultPrevented).toBe(true);
		expect(opened).toHaveLength(3);
		attachment.dispose();
	});
	test("physical Ctrl and Meta gestures remain platform-specific", () => {
		h = makeHarness();
		const target = new FakeEl("a", h.doc);
		expect(isTerminalLinkActivationGesture(clickEvent(target, { metaKey: true }), inactiveLinkActivation)).toBe(true);
		expect(isTerminalLinkActivationGesture(clickEvent(target, { ctrlKey: true }), inactiveLinkActivation)).toBe(false);
		h.nav.userAgent = "Linux";
		h.nav.platform = "Linux x86_64";
		expect(isTerminalLinkActivationGesture(clickEvent(target, { ctrlKey: true }), inactiveLinkActivation)).toBe(true);
		expect(isTerminalLinkActivationGesture(clickEvent(target, { metaKey: true }), inactiveLinkActivation)).toBe(false);
	});
});
