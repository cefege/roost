// The Deepgram engine's async continuations outlive the recording that issued
// them: a token grant, a socket handler, a repair and the start watchdog all
// resume long after a stop. `endIntent` cannot tell them apart — completeSend()
// resets it to null and null ALSO means "a recording is live" — so every
// continuation carries a run token instead. Contracts defended here: a stopped
// recording never opens a socket, a stale run cannot end or fail the NEXT
// recording, and a device open that never settles ends its recording with a
// reason instead of poisoning the mic singleton for the page's lifetime.
//
// Globals (navigator/window/AudioContext/AudioWorkletNode/WebSocket/URL) are
// stubbed the same way as audioPcmCapture.test.ts — both modules read them
// lazily, so a static import is safe.

import { expect, test, describe, beforeEach, afterAll } from "bun:test";
import { createRoot } from "solid-js";
import type { Dictation, DeepgramDictationOpts } from "../src/lib/deepgramDictation.ts";
import { createDeepgramDictation, dictationTimings } from "../src/lib/deepgramDictation.ts";
import {
	captureQuirks,
	micIdle,
	micTimeouts,
	releaseMic,
} from "../src/lib/audioPcmCapture.ts";

interface FakeTrack { stopped: boolean; stop: () => void }
interface FakeStream { getTracks: () => FakeTrack[]; track: FakeTrack }
function makeStream(): FakeStream {
	const track: FakeTrack = { stopped: false, stop() { this.stopped = true; } };
	return { getTracks: () => [track], track };
}

// per-test controllable state (test doubles for APIs bun doesn't provide)
let getUserMediaCalls = 0;
let getUserMediaImpl: () => Promise<unknown> = () => Promise.resolve(makeStream());

class FakeSource { connect() {} disconnect() {} }

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

// ── the Deepgram socket ───────────────────────────────────────────────────
// The engine reads OPEN/CLOSED off the GLOBAL WebSocket, assigns onopen /
// onmessage / onerror / onclose, and sends both JSON control frames and raw PCM
// ArrayBuffers, so all of that has to be real here.
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
	/** The server accepted the upgrade. */
	open() { this.readyState = FakeWS.OPEN; this.onopen?.(); }
	/** One Deepgram JSON frame. */
	deliver(msg: unknown) { this.onmessage?.({ data: JSON.stringify(msg) }); }
	/** A close frame nobody asked for. */
	drop(code: number, reason = "") { this.readyState = FakeWS.CLOSED; this.onclose?.({ code, reason }); }
	/** The transcript frame a stop() is waiting for. */
	finalResult(transcript: string) {
		this.deliver({
			type: "Results",
			is_final: true,
			from_finalize: true,
			channel: { alternatives: [{ transcript }] },
		});
	}
}

// ── global stubs ──────────────────────────────────────────────────────────
const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");
const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const originalAudioWorkletNode = Object.getOwnPropertyDescriptor(globalThis, "AudioWorkletNode");
const originalWebSocket = Object.getOwnPropertyDescriptor(globalThis, "WebSocket");
const originalCreateObjectURL = URL.createObjectURL;
const originalRevokeObjectURL = URL.revokeObjectURL;
const g = globalThis as unknown as {
	window: { AudioContext?: unknown; WebSocket?: unknown };
	URL: { createObjectURL: (b: unknown) => string; revokeObjectURL: (u: string) => void };
	AudioWorkletNode: unknown;
	WebSocket: unknown;
};
Object.defineProperty(globalThis, "navigator", {
	value: {
		userAgent: "bun-test",
		mediaDevices: { getUserMedia: () => { getUserMediaCalls++; return getUserMediaImpl(); } },
	},
	configurable: true, writable: true,
});
Object.defineProperty(globalThis, "window", { value: {}, configurable: true, writable: true });
let lastNode: FakeAudioWorkletNode | null = null;
class FakeAudioWorkletNode {
	port = { onmessage: null as ((e: { data: Float32Array }) => void) | null, close() {} };
	constructor() { lastNode = this; }
	connect() {}
	disconnect() {}
}
g.AudioWorkletNode = FakeAudioWorkletNode;
g.WebSocket = FakeWS;
g.URL.createObjectURL = () => "blob:fake";
g.URL.revokeObjectURL = () => {};

// 2048 samples @48 kHz clears the resampler's 40 ms emit threshold, so the sink
// really does receive a chunk.
function pushAudio(amplitude = 0.5): void {
	lastNode?.port.onmessage?.({ data: new Float32Array(2048).fill(amplitude) });
}

const flush = (ms = 0) => {
	const { promise, resolve } = Promise.withResolvers<void>();
	setTimeout(resolve, ms);
	return promise;
};
async function waitFor(cond: () => boolean, budgetMs = 2000): Promise<void> {
	const until = Date.now() + budgetMs;
	while (!cond()) {
		if (Date.now() > until) throw new Error("condition never became true");
		await flush(2);
	}
}

interface Harness {
	dict: Dictation;
	dispose: () => void;
	ends: number;
	failures: number;
}
function mount(
	opts: Pick<DeepgramDictationOpts, "grantToken">,
): Harness {
	const h = { ends: 0, failures: 0 } as Harness;
	h.dict = createRoot((dispose) => {
		h.dispose = dispose;
		return createDeepgramDictation({
			language: () => "en",
			keyterms: () => [],
			grantToken: opts.grantToken,
			onEnd: () => { h.ends++; },
			onFailure: () => { h.failures++; },
		});
	});
	return h;
}

describe("deepgramDictation run token", () => {
	beforeEach(() => {
		releaseMic(); // the mic pipeline is a singleton — every case starts cold
		micIdle.releaseMs = 60_000;
		micTimeouts.openMs = 6_000;
		micTimeouts.resumeMs = 1_500;
		micTimeouts.moduleMs = 3_000;
		captureQuirks.workletBroken = false;
		dictationTimings.finalizeWaitMs = 60;
		dictationTimings.silenceGraceMs = 10_000; // only case 3 wants a watchdog
		dictationTimings.startGraceMs = 10_000;
		sockets = [];
		lastNode = null;
		getUserMediaCalls = 0;
		getUserMediaImpl = () => Promise.resolve(makeStream());
		g.window.AudioContext = WorkletCtx;
		g.window.WebSocket = FakeWS;
	});

	afterAll(() => {
		releaseMic();
		if (originalNavigator) Object.defineProperty(globalThis, "navigator", originalNavigator);
		else Reflect.deleteProperty(globalThis, "navigator");
		if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
		else Reflect.deleteProperty(globalThis, "window");
		if (originalAudioWorkletNode) Object.defineProperty(globalThis, "AudioWorkletNode", originalAudioWorkletNode);
		else Reflect.deleteProperty(globalThis, "AudioWorkletNode");
		if (originalWebSocket) Object.defineProperty(globalThis, "WebSocket", originalWebSocket);
		else Reflect.deleteProperty(globalThis, "WebSocket");
		URL.createObjectURL = originalCreateObjectURL;
		URL.revokeObjectURL = originalRevokeObjectURL;
	});

	test("a stop before the socket opens never creates one", async () => {
		// Routine on cellular: the token grant + handshake lose the race to a
		// short utterance. The grant then resolved into a recording that had
		// already ended and opened a socket for it, on the SHARED ws field.
		const grant = Promise.withResolvers<string>();
		const h = mount({ grantToken: () => grant.promise });

		h.dict.start();
		h.dict.stop();
		expect(h.ends).toBe(1); // no socket to finalize against — ends at once

		grant.resolve("key");
		await flush();
		await flush();

		expect(sockets.length).toBe(0);
		expect(h.ends).toBe(1);
		expect(h.failures).toBe(0);
		h.dispose();
	});

	test("a stale run cannot kill the next recording", async () => {
		const grant1 = Promise.withResolvers<string>();
		const grant2 = Promise.withResolvers<string>();
		let grants = 0;
		const h = mount({
			grantToken: () => { grants++; return grants === 1 ? grant1.promise : grant2.promise; },
		});

		h.dict.start();
		h.dict.stop(); // recording #1 ends while its grant is still in flight

		// Recording #2 gets a real socket.
		h.dict.start();
		grant2.resolve("key");
		await waitFor(() => sockets.length === 1);
		sockets[0]!.open();

		// …and NOW recording #1's grant lands. It used to open a second socket
		// onto the shared field; that socket's close then ended the live
		// recording with "Deepgram connection dropped (network)".
		grant1.resolve("stale");
		await flush();
		await flush();
		expect(sockets.length).toBe(1);

		h.dict.stop();
		sockets[0]!.finalResult("hello from the mic");
		await flush();

		expect(h.ends).toBe(2);
		expect(h.failures).toBe(0);
		expect(h.dict.error()).toBeNull();
		expect(h.dict.final()).toBe("hello from the mic");
		h.dispose();
	});

	test("a device open that never settles ends the recording and leaves the mic usable", async () => {
		// The measured iOS 18.7 PWA case: getUserMedia parks forever, so
		// startCapture never resolves and the silence watch — armed FROM that
		// resolution — is never armed. Nothing was watching the recording at all.
		dictationTimings.startGraceMs = 20;
		const gum = Promise.withResolvers<unknown>();
		const late = makeStream();
		getUserMediaImpl = () => gum.promise;
		const h = mount({ grantToken: () => Promise.resolve("key") });

		h.dict.start();
		await waitFor(() => h.failures === 1);
		expect(h.dict.error()).toBeTruthy();

		// WebKit hands the device over afterwards; it must arrive as an orphan
		// that gets stopped, not a live device nobody will ever close.
		gum.resolve(late);
		await flush();
		expect(late.track.stopped).toBe(true);

		// The next tap must open a FRESH device instead of awaiting the same dead
		// promise — this is "the first recording works and every one after it is
		// dead", in one assertion.
		getUserMediaImpl = () => Promise.resolve(makeStream());
		const beforeCalls = getUserMediaCalls;
		const beforeSockets = sockets.length; // the failed run had a socket of its own
		h.dict.start();
		await waitFor(() => sockets.length === beforeSockets + 1);
		const live = sockets[sockets.length - 1]!;
		live.open();
		await waitFor(() => lastNode !== null);
		expect(getUserMediaCalls).toBeGreaterThan(beforeCalls);

		// Poll: the sink attaches a tick after the pipeline publishes, and frames
		// pushed before that are dropped by design (a warm idle mic buffers
		// nothing).
		await waitFor(() => {
			pushAudio(0.5);
			return live.sent.some((v) => v instanceof ArrayBuffer);
		});
		h.dispose();
	});
});
