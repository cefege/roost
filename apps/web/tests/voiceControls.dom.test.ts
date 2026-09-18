// The controller seam over the composer's microphone, locked at the COMPONENT
// level: the mounted MobileVoiceInput must publish its own toggle/discard
// closures (one implementation, never a synthetic click on a testid), the
// registration must survive the per-pane mount order, and a start that carries
// no user activation must be refused rather than faked.
//
// This repo runs no jsdom and Solid resolves to its SSR build under `bun test`,
// so the component is mounted the way mobileVoiceInput.mic-state.dom.test.ts
// does it: remock `solid-js` onto its CLIENT dist build, give bun's classic and
// automatic JSX lowerings a virtual renderer, and mock the engine/transport
// modules. Only the seam is asserted here, so the M3 primitives and the audio
// pipeline are stubbed instead of faked in detail.
//
// Case order is load-bearing: the permission latch in lib/voiceState.ts is
// module state, and reaching a live recording sets it for the rest of the file,
// so the before-any-grant case must run first.

import { describe, test, expect, mock, beforeEach, afterEach } from "bun:test";
import type * as SolidApi from "solid-js";

// ── globals the component touches ───────────────────────────────────────────

const ls: Record<string, string> = {};
(globalThis as unknown as { localStorage: Storage }).localStorage = {
	getItem: (k: string) => ls[k] ?? null,
	setItem: (k: string, v: string) => { ls[k] = v; },
	removeItem: (k: string) => { delete ls[k]; },
	clear: () => { for (const k of Object.keys(ls)) delete ls[k]; },
	key: () => null,
	length: 0,
} as Storage;

const g = globalThis as unknown as Record<string, unknown>;
// No SpeechRecognition and no navigator.permissions: the Web Speech fallback
// stays out of the way, and the permission probe must degrade to "needs a tap"
// on an engine that does not answer the microphone descriptor.
g.window = { isSecureContext: true };
Object.defineProperty(globalThis, "navigator", { configurable: true, value: {} });

// ── solid client build + virtual renderer ───────────────────────────────────

// @ts-expect-error TS7016 — solid ships no types for its dist entries.
const S = await import("solid-js/dist/solid.js") as unknown as typeof SolidApi;
mock.module("solid-js", () => ({ ...S }));

interface VNode { tag: unknown; props: Record<string, unknown> }
function createElement(tag: unknown, props: Record<string, unknown> | null, ...kids: unknown[]): VNode {
	const merged = { ...(props ?? {}) };
	if (kids.length > 0) merged.children = kids.length === 1 ? kids[0] : kids;
	return { tag, props: merged };
}
g.React = { Fragment: Symbol("Fragment"), createElement };
mock.module("react/jsx-dev-runtime", () => ({
	Fragment: Symbol("Fragment"),
	jsxDEV: (tag: unknown, props: Record<string, unknown> | null) => createElement(tag, props),
}));

// ── engine / transport / primitive stubs ────────────────────────────────────

interface FakeEngineOptions {
	onEnd: () => void;
	onFailure: () => void;
	onLive: () => void;
}
interface FakeEngine {
	options: FakeEngineOptions;
	starts: number;
	stops: number;
	aborts: number;
	resets: number;
}
const engines: FakeEngine[] = [];
let engineFinalText = "hello pad";

mock.module("../src/lib/deepgramDictation.ts", () => ({
	createDeepgramDictation: (options: FakeEngineOptions) => {
		const engine: FakeEngine = { options, starts: 0, stops: 0, aborts: 0, resets: 0 };
		engines.push(engine);
		return {
			supported: true,
			final: () => engineFinalText,
			interim: () => "",
			error: () => null,
			start: () => { engine.starts++; },
			stop: () => { engine.stops++; },
			abort: () => { engine.aborts++; },
			reset: () => { engine.resets++; },
		};
	},
}));

let micWarm = false;
mock.module("../src/lib/audioPcmCapture.ts", () => ({
	isMicWarm: () => micWarm,
	micIdle: { releaseMs: 0 },
	warmMic: async () => {},
}));
mock.module("../src/lib/deepgramKey.ts", () => ({
	getDeepgramKey: async () => "test-key",
	invalidateDeepgramKey: () => {},
	prefetchDeepgramKey: () => {},
}));
mock.module("../src/lib/windowSizeClass.ts", () => ({ isTouchDevice: () => false }));
mock.module("../src/components/Settings/md/primitives.tsx", () => ({
	IconButton: (props: Record<string, unknown>) => props,
}));
mock.module("../src/connect.ts", () => ({
	coordClient: {
		transcriptionGetConfig: async () => ({ deepgramConfigured: true, deepgramLanguage: "en" }),
	},
}));

// Dynamic imports are REQUIRED: the mocks above must be installed before the
// component and lib modules bind their dependencies.
const { MobileVoiceInput } = await import("../src/components/MobileVoiceInput.tsx");
const {
	activeVoiceOwner,
	ensureTranscriptionConfig,
	micStartableWithoutGesture,
	setActiveVoiceOwner,
	transcriptionConfig,
	voiceControls,
	voiceDictating,
} = await import("../src/lib/voiceState.ts");

// ── harness ─────────────────────────────────────────────────────────────────

const flush = (ms = 0) => new Promise<void>((r) => setTimeout(r, ms));

interface Mounted {
	dispose: () => void;
	engine: FakeEngine;
	transcripts: string[];
	discards: number;
}
const mounted: Mounted[] = [];

function mountComposer(sessionId: string): Mounted {
	const transcripts: string[] = [];
	const handle = { dispose: () => {}, engine: {} as FakeEngine, transcripts, discards: 0 };
	S.createRoot((dispose) => {
		handle.dispose = dispose;
		(MobileVoiceInput as unknown as (props: Record<string, unknown>) => unknown)({
			ownerId: sessionId,
			active: true,
			onTranscript: (text: string) => { transcripts.push(text); },
			onLiveTranscript: () => {},
			onDiscard: () => { handle.discards++; },
		});
	});
	handle.engine = engines[engines.length - 1]!;
	mounted.push(handle);
	return handle;
}

/** The registered controls, which every case reaches the component through. */
function controls() {
	const registered = voiceControls();
	if (!registered) throw new Error("no voice controls registered");
	return registered;
}

// Deepgram must be the selected engine before the first case: the config RPC is
// module-cached and async, so settle it once here.
ensureTranscriptionConfig();
await flush(0);
expect(transcriptionConfig()?.deepgramConfigured).toBe(true);

beforeEach(() => {
	engines.length = 0;
	engineFinalText = "hello pad";
});

afterEach(async () => {
	while (mounted.length > 0) mounted.pop()?.dispose();
	await flush(0);
	setActiveVoiceOwner(null);
});

describe("a controller start with no user activation", () => {
	test("is refused before any grant, and leaves the composer idle", () => {
		micWarm = false;
		const composer = mountComposer("session-cold");

		expect(micStartableWithoutGesture()).toBe(false);
		expect(controls().canStartWithoutGesture()).toBe(false);

		controls().toggle();

		// Nothing opened, nothing claimed the voice slot: no fake listening state
		// for a getUserMedia the browser would deny without even prompting.
		expect(composer.engine.starts).toBe(0);
		expect(activeVoiceOwner()).toBeNull();
		expect(voiceDictating()).toBe(false);
	});

	test("is allowed once the pipeline is warm from a trusted tap", () => {
		micWarm = true;
		mountComposer("session-warm");

		expect(controls().canStartWithoutGesture()).toBe(true);
	});
});

describe("registered controls reach the component's own closures", () => {
	test("toggle starts, then stops AND sends — never discards speech", async () => {
		micWarm = true;
		const composer = mountComposer("session-send");

		controls().toggle();
		expect(composer.engine.starts).toBe(1);
		expect(voiceDictating()).toBe(true);

		composer.engine.options.onLive(); // device + transport live → listening
		controls().toggle();

		// stopAndSend, not the discard path: the engine is stopped, never aborted.
		expect(composer.engine.stops).toBe(1);
		expect(composer.engine.aborts).toBe(0);
		expect(voiceDictating()).toBe(true); // finalizing still owns the slot

		composer.engine.options.onEnd();
		await flush(0);
		expect(composer.transcripts).toEqual(["hello pad"]);
		expect(voiceDictating()).toBe(false);
	});

	test("discard aborts the engine and never delivers a transcript", () => {
		micWarm = true;
		const composer = mountComposer("session-discard");

		controls().toggle();
		composer.engine.options.onLive();
		controls().discard();

		expect(composer.engine.aborts).toBe(1);
		expect(composer.discards).toBe(1);
		expect(composer.transcripts).toEqual([]);
		expect(voiceDictating()).toBe(false);
	});

	test("a grant carries over: canStartWithoutGesture survives a cold pipeline", () => {
		// The previous cases reached a live recording, which latches the grant.
		micWarm = false;
		mountComposer("session-latched");

		expect(controls().canStartWithoutGesture()).toBe(true);
	});
});

describe("registration identity across per-pane remounts", () => {
	test("the outgoing instance's cleanup leaves the live registration intact", () => {
		micWarm = true;
		const first = mountComposer("session-first");
		const second = mountComposer("session-second");

		// A pane focus switch mounts the next composer BEFORE the old one's
		// cleanup runs; an unconditional clear here would kill the pad's mic.
		first.dispose();
		mounted.splice(mounted.indexOf(first), 1);

		controls().discard();
		expect(second.discards).toBe(1);
		expect(first.discards).toBe(0);
	});

	test("unmounting the live instance clears the registration", () => {
		micWarm = true;
		const composer = mountComposer("session-last");
		expect(voiceControls()).not.toBeNull();

		composer.dispose();
		mounted.splice(mounted.indexOf(composer), 1);

		expect(voiceControls()).toBeNull();
	});
});

describe("voiceDictating", () => {
	test("tracks the active voice owner reactively", () => {
		const seen: boolean[] = [];
		// Effects flush when the root's synchronous body ends, so the owner writes
		// belong outside it — each one is its own update.
		const dispose = S.createRoot((disposeRoot) => {
			S.createEffect(() => { seen.push(voiceDictating()); });
			return disposeRoot;
		});
		expect(seen).toEqual([false]);

		setActiveVoiceOwner({ sessionId: "session-owner" as never, token: 99 });
		expect(seen).toEqual([false, true]);

		setActiveVoiceOwner(null);
		expect(seen).toEqual([false, true, false]);
		dispose();
	});
});
