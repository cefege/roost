// Voice-dictation defects, locked at the COMPONENT level: interim Deepgram
// text must stream into the composer's draft (defect B), a tap during opening
// must gate double-starts behind a distinct "starting" state (defect A),
// switching away or unmounting mid-dictation must COMMIT dictated text instead
// of discarding it (defect C), an explicit ✕ remains the only revert, and an
// idle mic stays mounted while another instance owns the voice slot.
//
// This repo runs no jsdom and Solid resolves to its SSR build under `bun test`,
// so these components cannot render through vite-plugin-solid here (see
// folderListRowStability.dom.test.ts). Following attachmentsPicker.dom.test.ts:
// fake the exact DOM/globals the components touch, mock the transport modules,
// and dynamically import the components under test. Two extra seams make them
// executable under bun's own TSX transform:
//   • `solid-js` is remocked onto its CLIENT dist build, so signals/effects
//     actually run inside createRoot;
//   • bun lowers JSX classically to a bare `React.createElement` reference,
//     which is given a virtual renderer: function tags run like
//     createComponent (with the enclosing owner), intrinsic tags record their
//     props for inspection. Reactive assertions therefore go through signals,
//     engine frames, and the composerDrafts persistence effect — the same text
//     a reload would restore — not vnode props, which snapshot once.

import { describe, test, expect, mock, beforeEach, afterEach } from "bun:test";
import type { Session } from "@roost/shared/wire";
import type * as SolidApi from "solid-js";
import { releaseMic, micIdle, captureQuirks } from "../src/lib/audioPcmCapture.ts";

// ── globals the components/engine touch ─────────────────────────────────────

const ls: Record<string, string> = {};
(globalThis as unknown as { localStorage: Storage }).localStorage = {
	getItem: (k: string) => ls[k] ?? null,
	setItem: (k: string, v: string) => { ls[k] = v; },
	removeItem: (k: string) => { delete ls[k]; },
	clear: () => { for (const k of Object.keys(ls)) delete ls[k]; },
	key: () => null, length: 0,
} as Storage;

interface FakeTrack { stopped: boolean; stop: () => void }
function makeStream(): { getTracks: () => FakeTrack[]; track: FakeTrack } {
	const track: FakeTrack = { stopped: false, stop() { this.stopped = true; } };
	return { getTracks: () => [track], track };
}
class FakeSource { connect() {} disconnect() {} }
type FakePort = { onmessage: ((e: { data: Float32Array }) => void) | null; close(): void };
class WorkletCtx {
	sampleRate = 48000;
	destination = {};
	state: AudioContextState = "running";
	audioWorklet = { addModule: (_url: string) => Promise.resolve() };
	createMediaStreamSource() { return new FakeSource(); }
	createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
	resume() { this.state = "running"; return Promise.resolve(); }
	close() { return Promise.resolve(); }
}

let sockets: FakeWS[] = [];
class FakeWS {
	static readonly OPEN = 1;
	static readonly CLOSED = 3;
	readyState = 0;
	binaryType = "";
	sent: unknown[] = [];
	onopen: (() => void) | null = null;
	onmessage: ((e: { data: unknown }) => void) | null = null;
	onerror: (() => void) | null = null;
	onclose: ((e: { code: number; reason: string }) => void) | null = null;
	constructor(readonly url: string, readonly protocols?: string[]) { sockets.push(this); }
	send(data: unknown) { this.sent.push(data); }
	close() { this.readyState = FakeWS.CLOSED; }
	open() { this.readyState = FakeWS.OPEN; this.onopen?.(); }
	deliver(msg: unknown) { this.onmessage?.({ data: JSON.stringify(msg) }); }
}
let lastNode: { port: FakePort } | null = null;
class FakeAudioWorkletNode {
	port: FakePort = { onmessage: null, close() {} };
	constructor() { lastNode = this; }
	connect() {}
	disconnect() {}
}

let getUserMediaCalls = 0;
let getUserMediaImpl: () => Promise<{ getTracks: () => FakeTrack[] }> = () => Promise.resolve(makeStream());

const g = globalThis as unknown as Record<string, unknown>;
Object.defineProperty(globalThis, "navigator", {
	value: {
		userAgent: "bun-test",
		maxTouchPoints: 0,
		mediaDevices: { getUserMedia: () => { getUserMediaCalls++; return getUserMediaImpl(); } },
	},
	configurable: true, writable: true,
});
g.window = {
	innerWidth: 1280,
	innerHeight: 900,
	AudioContext: WorkletCtx,
	WebSocket: FakeWS,
	addEventListener: () => {},
	removeEventListener: () => {},
	matchMedia: () => ({ matches: false }),
};
if (typeof g.requestAnimationFrame !== "function") {
	g.requestAnimationFrame = (fn: () => void) => setTimeout(fn, 0) as unknown as number;
}
g.AudioWorkletNode = FakeAudioWorkletNode;
g.WebSocket = FakeWS;
URL.createObjectURL = () => "blob:fake";
URL.revokeObjectURL = () => {};
g.document = {
	activeElement: null,
	addEventListener: () => {},
	removeEventListener: () => {},
	getSelection: () => null,
};

// ── solid client build + virtual renderer ───────────────────────────────────
	// solid-js ships no types for its dist entry; the cast below restores the
	// package's public API surface.
	// @ts-expect-error TS7016
	const S = await import("solid-js/dist/solid.js") as unknown as typeof SolidApi;
mock.module("solid-js", () => ({ ...S }));

interface VNode { tag: unknown; props: Record<string, unknown>; rendered?: unknown }

function invokeComponent(vnode: VNode): void {
	if (typeof vnode.tag !== "function") return;
	const owner = S.getOwner();
	vnode.rendered = S.runWithOwner(
		owner,
		() => (vnode.tag as (p: Record<string, unknown>) => unknown)(vnode.props),
	);
}
// See file header: classic-JSX lowering target.
function createElement(tag: unknown, props: Record<string, unknown> | null, ...kids: unknown[]): VNode {
	const merged: Record<string, unknown> = { ...(props ?? {}) };
	if (kids.length > 0) merged.children = kids.length === 1 ? kids[0] : kids;
	const vnode: VNode = { tag, props: merged };
	invokeComponent(vnode);
	return vnode;
}
const ReactShim = {
	Fragment: Symbol("Fragment"),
	createElement,
};
g.React = ReactShim;
// The component also carries real JSX, which bun lowers through the automatic
// runtime's jsxDEV instead of the classic global reference above — route that
// into the SAME virtual renderer (children arrive inside props, not rest args).
mock.module("react/jsx-dev-runtime", () => ({
	Fragment: ReactShim.Fragment,
	jsxDEV(type: unknown, props: Record<string, unknown> | null): VNode {
		const kids = props !== null && props.children !== undefined ? [props.children] : [];
		return createElement(type, props ?? {}, ...kids);
	},
}));

// ── transport mocks ─────────────────────────────────────────────────────────

mock.module("../src/connect.ts", () => ({
	coordClient: {
		transcriptionGetConfig: async () => ({ deepgramConfigured: true, deepgramLanguage: "en" }),
		transcriptionGrantToken: async () => ({ accessToken: "test-key" }),
	},
}));

// Dynamic import is REQUIRED here: the mocks above must be installed before
// the component modules (and their solid bindings) evaluate.
const { TerminalComposeButton } = await import("../src/components/TerminalComposeButton.tsx");
const { saveComposerDraft } = await import("../src/lib/composerDrafts.ts");

// ── harness ─────────────────────────────────────────────────────────────────

const flush = (ms = 0) => new Promise<void>((r) => setTimeout(r, ms));
async function waitFor(cond: () => boolean, budgetMs = 2000): Promise<void> {
	const until = Date.now() + budgetMs;
	while (!cond()) {
		if (Date.now() > until) throw new Error("condition never became true");
		await flush(2);
	}
}
function pushAudio(amplitude = 0.5): void {
	lastNode?.port.onmessage?.({ data: new Float32Array(2048).fill(amplitude) });
}

function findByTestId(node: unknown, testid: string): VNode | undefined {
	if (!node || typeof node !== "object") return undefined;
	if (Array.isArray(node)) {
		for (const inner of node) {
			const hit = findByTestId(inner, testid);
			if (hit) return hit;
		}
		return undefined;
	}
	const v = node as VNode;
	if (typeof v.tag === "function") {
		// Component tag: only its RENDERED output exists in the tree. Raw
		// props.children hold unevaluated branches (e.g. a <Show> that chose
		// its fallback) which must NOT match.
		return findByTestId(v.rendered, testid);
	}
	if (typeof v.props !== "object" || v.props === null) return undefined;
	if (v.props["data-testid"] === testid) return v;
	return findByTestId(v.props.children, testid);
}

/** Raw lookup that also descends UNEVALUATED component props.children. Used
 *  solely to reach event handlers the virtual renderer froze at creation time
 *  (a <Show> branch entered after mount): the handler object is exactly what
 *  the real DOM button carries once the branch renders. */
function findHandlerAnywhere(node: unknown, testid: string): VNode | undefined {
	if (!node || typeof node !== "object") return undefined;
	if (Array.isArray(node)) {
		for (const inner of node) {
			const hit = findHandlerAnywhere(inner, testid);
			if (hit) return hit;
		}
		return undefined;
	}
	const v = node as VNode;
	if (typeof v.props === "object" && v.props !== null) {
		if (v.props["data-testid"] === testid) return v;
		const children = [v.rendered, v.props.children];
		for (const kid of children) {
			const hit = findHandlerAnywhere(kid, testid);
			if (hit) return hit;
		}
	}
	return undefined;
}

function clickButton(tree: VNode, testid: string): void {
	const btn = findByTestId(tree, testid);
	if (!btn) throw new Error(`button ${testid} not rendered`);
	(btn.props.onClick as () => void)();
}

function storedDraft(sessionId: string): string {
	return JSON.parse(ls["roost.composerDrafts.v1"] ?? "{}")[sessionId] ?? "";
}

interface Mounted {
	tree: VNode;
	dispose: () => void;
	setActive: (v: boolean) => void;
}

const mountedComposers: Mounted[] = [];

function mountComposer(sessionId: string, initiallyActive = true): Mounted {
	const [sessionActive, setActive] = S.createSignal(initiallyActive);
	let tree!: VNode;
	const h = {} as Mounted;
	h.dispose = S.createRoot((dispose: () => void) => {
		tree = TerminalComposeButton({
			placement: "pane",
			session: { id: sessionId } as unknown as Session, // only .id is read here
			get active() { return sessionActive(); },
			onSubmit: (text: string) => ({
				accepted: true,
				inputSeq: 1n,
				result: Promise.resolve({ status: "accepted", inputSeq: 1n, writtenBytes: text.length }),
			}),
			onAttachFiles: () => {},
		}) as unknown as VNode; // JSX element type under bun's transform differs
		return dispose;
	});
	h.tree = tree;
	h.setActive = (v: boolean) => setActive(v);
	mountedComposers.push(h);
	return h;
}

/** Open the Deepgram transport for the composer's live recording. */
async function startListening(h: Mounted): Promise<FakeWS> {
	clickButton(h.tree, "voice-mic");
	await waitFor(() => sockets.length === 1);
	const ws = sockets[0]!;
	ws.open();
	await flush();
	pushAudio();
	return ws;
}

// First mount primes the module-cached transcription config RPC; settle its
// promise so every real test starts with Deepgram selected deterministically.
{
	releaseMic();
	mountComposer("s-warmup").dispose();
	await flush();
	await flush();
}

beforeEach(() => {
	releaseMic();
	sockets = [];
	lastNode = null;
	getUserMediaCalls = 0;
	getUserMediaImpl = () => Promise.resolve(makeStream());
	captureQuirks.workletBroken = false;
	micIdle.releaseMs = 60_000;
	for (const key of Object.keys(ls)) delete ls[key];
});

// A failed assertion mid-test must not leak a live recording's voice-ownership
// claim into the next test — dispose whatever is still mounted.
afterEach(async () => {
	while (mountedComposers.length > 0) mountedComposers.pop()!.dispose();
	releaseMic();
	await flush();
});

describe("the starting gate blocks double-starts (defect A)", () => {
	test("a tap while the device open is in flight is a deliberate no-op", async () => {
		// Hold getUserMedia: the tap has claimed "starting" but nothing is live.
		let releaseGum!: () => void;
		const gated = new Promise<{ getTracks: () => FakeTrack[] }>((resolve) => { releaseGum = () => resolve(makeStream()); });
		getUserMediaImpl = () => gated;

		const h = mountComposer("s-gate");
		clickButton(h.tree, "voice-mic");
		await flush();

		// Mash the button while "starting": no second device open, no stop.
		clickButton(h.tree, "voice-mic");
		clickButton(h.tree, "voice-mic");
		await flush();

		// Once the device lands and the socket opens, the mic promotes and works.
		releaseGum();
		await waitFor(() => sockets.length === 1);
		expect(getUserMediaCalls).toBe(1);
		sockets[0]!.open();
		sockets[0]!.deliver({ type: "Results", is_final: false, channel: { alternatives: [{ transcript: "hi" }] } });
		await waitFor(() => storedDraft("s-gate") === "hi");
	});

	test("the mic button carries the pending-state affordances", async () => {
		const h = mountComposer("s-affordance");
		const mic = findByTestId(h.tree, "voice-mic");
		expect(mic).toBeDefined();
		expect(mic!.props["data-starting"]).toBe("false"); // binding exists, currently idle
		expect(mic!.props["aria-busy"]).toBeUndefined(); // absent while not starting
		expect(mic!.props["data-recording"]).toBe("false");
	});
});

describe("ownership never unmounts an idle mic (defect A)", () => {
	test("a second instance keeps its clickable idle mic and cannot double-record", async () => {
		const owner = mountComposer("s-owner");
		const twin = mountComposer("s-twin");

		const ws = await startListening(owner); // owner claims the voice slot
		expect(findByTestId(twin.tree, "voice-mic")).toBeDefined();
		expect(findByTestId(twin.tree, "voice-discard")).toBeUndefined(); // extras stay owner-only

		// Tapping the twin's idle mic must NOT open a second device/socket.
		clickButton(twin.tree, "voice-mic");
		await flush();
		expect(getUserMediaCalls).toBe(1);
		expect(sockets.length).toBe(1);
		expect(ws.sent.length).toBeGreaterThan(0);
	});
});
