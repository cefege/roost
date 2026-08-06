// audioPcmCapture — the page's single microphone pipeline: getUserMedia →
// AudioWorklet → linear16 PCM @ 16 kHz mono, emitted as ~40 ms Int16Array
// chunks (what Deepgram's WS wants: encoding=linear16, sample_rate=16000).
// The pipeline is a module singleton held WARM between recordings, because a
// per-recording device open costs 1–2 s and swallowed the first spoken words;
// startCapture/stopCapture only attach and detach the sink, and the device
// closes on an idle timer, on tab-hide, or on an explicit releaseMic().
// Callers: deepgramDictation.ts (capture), MobileVoiceInput.tsx (warm on tap).

const TARGET_RATE = 16000;
const CHUNK_MS = 40;

// Worklet processor: forwards raw Float32 mono frames to the main thread, which
// resamples to 16 kHz. Kept minimal so the audio render thread never stalls.
const WORKLET_SRC = `
class PcmForwarder extends AudioWorkletProcessor {
  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (ch && ch.length) this.port.postMessage(ch.slice(0));
    return true;
  }
}
registerProcessor('pcm-forwarder', PcmForwarder);
`;

export type PcmSink = (pcm16: Int16Array) => void;

/** Which node is actually pulling audio out of the graph. */
export type CapturePath = "worklet" | "scriptprocessor" | "none";

/** What the pipeline has really delivered since the current sink attached —
 *  the evidence a caller needs to tell a live recording from a dead one. */
export interface CaptureStats {
	/** Audio callbacks received since attach. 0 = the graph is not running. */
	frames: number;
	/** Largest |sample| seen since attach. 0 with frames > 0 = digital silence
	 *  (device muted, or the OS handed us a dead input) rather than a dead graph. */
	peak: number;
	path: CapturePath;
	/** iOS reports "interrupted" here after a call/lock; "suspended" after a
	 *  backgrounding that resume() failed to undo. */
	ctxState: string;
	sampleRate: number;
}

/** How long the mic stays open after a recording ends, so the next tap reuses
 *  a live device. A mutable object, not a bare const, so tests can shrink it. */
export const micIdle = { releaseMs: 60_000 };

// ── pipeline state (one per page) ─────────────────────────────────────────
let ctx: AudioContext | null = null;
let stream: MediaStream | null = null;
let node: AudioWorkletNode | null = null;
// eslint-disable-next-line @typescript-eslint/no-deprecated
let proc: ScriptProcessorNode | null = null;
let source: MediaStreamAudioSourceNode | null = null;
let workletUrl: string | null = null;
let sink: PcmSink | null = null;
let warming: Promise<void> | null = null;
let idleTimer: ReturnType<typeof setTimeout> | null = null;
// Bumped by releaseMic: an in-flight warm-up that sees it changed abandons its
// half-built pipeline instead of writing into state that was just torn down.
let generation = 0;
// Bumped by stopCapture AND releaseMic: a startCapture still awaiting the
// warm-up must not attach its sink after the recording it belongs to ended.
let captureEpoch = 0;
let lastWarmupMs = 0;
// startCapture() calls that have begun but not yet attached their sink. The
// AUTOMATIC releases (idle timer, tab hide) must skip that window: `sink` is
// still null, so `!sink` alone reads a STARTING recording as an idle mic and
// tears the pipeline down under it — the recording then runs forever with zero
// audio and no error. An explicit releaseMic() is deliberately unaffected.
let startingCaptures = 0;

// ── liveness (the mobile silent-mic class) ────────────────────────────────
// A pipeline can be fully built and deliver NOTHING. Two documented WebKit
// behaviours produce exactly that on a phone while desktop is unaffected:
// the AudioContext is suspended/"interrupted" on backgrounding, lock or a call
// and resume() does not always restore it (webkit.org/b/237878), and
// AudioWorklet has shipped dead on iPhone-only builds (Apple DTS 768347).
// From here both look identical to a healthy recording — warm pipeline,
// attached sink, zero frames — so audio delivery is MEASURED, never assumed.
let framesSinceAttach = 0;
let peakSinceAttach = 0;
let capturePath: CapturePath = "none";

/** Device facts that outlive a single recording. `workletBroken` latches when
 *  repairCapture() had to fall back, so no later recording pays 2.5 s of dead
 *  audio to rediscover the same broken AudioWorklet. A mutable object, not a
 *  module `let`, so tests can reset the latch between cases. */
export const captureQuirks = { workletBroken: false };

// Linear-interpolation resampler state (inputRate → 16 kHz). Fractional read
// position carries across chunks so chunk boundaries don't drop/dup samples.
let inRate = 48000;
let readPos = 0;
let pending: Float32Array[] = [];
let pendingLen = 0;
const samplesPerChunk = Math.round((TARGET_RATE * CHUNK_MS) / 1000); // 640

// ── exported API ──────────────────────────────────────────────────────────

/** Opens the mic + AudioContext + worklet, or revives/reuses the pipeline when
 *  it is already warm. Idempotent; concurrent calls share one in-flight promise.
 *  Rejects with the raw getUserMedia error. */
export async function warmMic(): Promise<void> {
	clearIdle();
	if (isMicWarm()) {
		lastWarmupMs = 0;
		try {
			await resumeContext();
		} finally {
			armIdle();
		}
		// iOS suspends the context on backgrounding, lock, a call or a Siri
		// interruption, and resume() does NOT always bring it back
		// (webkit.org/b/237878). A suspended context renders nothing, so a
		// "warm" pipeline built on one is a mic that records silence with no
		// error at all — throw the graph away and open a fresh one instead.
		// discardPipeline(), NOT releaseMic(): the caller may be the startCapture
		// awaiting this very warm-up, and bumping captureEpoch would cancel the
		// attach we are trying to save — the tap would fail instead of healing.
		if (!ctx || ctx.state === "running") return;
		discardPipeline();
	}
	if (warming) return warming;
	warming = openPipeline().finally(() => {
		warming = null;
		armIdle();
	});
	return warming;
}

/** Routes captured PCM to `nextSink`, warming the pipeline first when cold.
 *  Resets the resampler so each recording starts clean. Resolves TRUE when the
 *  sink is attached; FALSE when the recording was superseded before the
 *  pipeline came up (stopCapture, or an explicit releaseMic). Rejects with the
 *  raw getUserMedia / AudioContext error. A silent `false` is what made a dead
 *  mic look like a live one — the caller must treat it as a failed start. */
export async function startCapture(nextSink: PcmSink): Promise<boolean> {
	const epoch = ++captureEpoch;
	startingCaptures++;
	try {
		await warmMic();
		// A releaseMic() that landed *before* this call but during an earlier
		// warm-up cancelled the pipeline we just awaited: the promise resolves
		// with nothing open. Re-open once rather than leave a silently dead mic.
		// (A release AFTER this call bumped captureEpoch and is honoured below.)
		if (epoch === captureEpoch && !isMicWarm()) await warmMic();
		// Stopped or released while warming — attaching now would stream a
		// finished recording's audio into a dead sink.
		if (epoch !== captureEpoch || !isMicWarm()) return false;
		readPos = 0;
		pending = [];
		pendingLen = 0;
		framesSinceAttach = 0;
		peakSinceAttach = 0;
		sink = nextSink;
		clearIdle(); // a live recording is never on the release clock
		return true;
	} finally {
		startingCaptures--;
	}
}

/** Detaches the sink and arms the idle-release timer. The mic stays open. */
export function stopCapture(): void {
	captureEpoch++;
	sink = null;
	pending = [];
	pendingLen = 0;
	armIdle();
}

/** Full teardown now: disconnect nodes, stop tracks, close ctx, revoke URL.
 *  Cancels any warm-up in flight and any sink attach still awaiting one. */
export function releaseMic(): void {
	generation++;
	captureEpoch++;
	clearIdle();
	disposePipeline();
}

// Throw away an unusable pipeline WITHOUT cancelling recordings: it invalidates
// any in-flight warm-up (generation) but leaves captureEpoch alone, so a
// startCapture awaiting the warm-up still attaches to the replacement. The
// difference matters — bumping the epoch here turns "revive a dead pipeline"
// into "fail the tap that asked for it".
function discardPipeline(): void {
	generation++;
	clearIdle();
	disposePipeline();
}

/** The AUTOMATIC release paths (idle timer, tab hide). A recording that is
 *  still coming up has no sink yet, so it must be counted here or the release
 *  kills it silently. `releaseMic()` stays the unconditional teardown. */
export function releaseMicIfIdle(): void {
	if (sink || startingCaptures > 0) return;
	releaseMic();
}

/** What this pipeline has actually delivered since the current sink attached.
 *  A recording with `frames === 0` is dead however healthy the rest looks. */
export function captureStats(): CaptureStats {
	return {
		frames: framesSinceAttach,
		peak: peakSinceAttach,
		path: capturePath,
		ctxState: ctx?.state ?? "none",
		sampleRate: ctx?.sampleRate ?? 0,
	};
}

/** Last resort for a recording that is attached but receiving nothing: throw
 *  the whole graph away, reopen it cold and re-attach `nextSink`. The retry
 *  skips the AudioWorklet from here on — a worklet that produced no frames is
 *  the iPhone-only dead-worklet bug, and the ScriptProcessor path is not
 *  affected by it. Resolves TRUE when the fresh pipeline attached; the caller
 *  must still re-check captureStats() before believing audio flows. */
export async function repairCapture(nextSink: PcmSink): Promise<boolean> {
	captureQuirks.workletBroken = true;
	releaseMic();
	return await startCapture(nextSink);
}

// Disposal WITHOUT the cancellation bumps: a warm-up that discovers it was
// cancelled must drop its half-built pipeline without also invalidating a
// startCapture that arrived after the cancellation and is waiting to re-open.
function disposePipeline(): void {
	sink = null;
	// Detach BEFORE closing: close() throwing would otherwise leave a discarded
	// worklet still posting into onFloat, and the next recording's sink would
	// receive two interleaved graphs' audio (a repaired pipeline runs a second
	// graph in the same page).
	try { if (node) node.port.onmessage = null; } catch { /* ignore */ }
	try { node?.port.close(); } catch { /* ignore */ }
	try { if (proc) proc.onaudioprocess = null; } catch { /* ignore */ }
	try { source?.disconnect(); } catch { /* ignore */ }
	try { node?.disconnect(); } catch { /* ignore */ }
	try { proc?.disconnect(); } catch { /* ignore */ }
	try { stream?.getTracks().forEach((t) => t.stop()); } catch { /* ignore */ }
	try { void ctx?.close(); } catch { /* ignore */ }
	if (workletUrl) { try { URL.revokeObjectURL(workletUrl); } catch { /* ignore */ } }
	ctx = null; stream = null; node = null; proc = null; source = null; workletUrl = null;
	pending = []; pendingLen = 0; readPos = 0;
	capturePath = "none";
}

export function isMicWarm(): boolean {
	return !!ctx && !!stream && !!(node || proc);
}

/** Duration of the most recent cold open in ms; 0 when the last warmMic() hit
 *  an already-warm pipeline. Feeds the voice.start_timing diag. */
export function micWarmupMs(): number {
	return lastWarmupMs;
}

// ── internals ─────────────────────────────────────────────────────────────

function resumeContext(): Promise<void> {
	if (!ctx || ctx.state === "running") return Promise.resolve();
	return ctx.resume();
}

function clearIdle(): void {
	if (idleTimer) {
		clearTimeout(idleTimer);
		idleTimer = null;
	}
}

// Puts an idle-but-open pipeline on the release clock. Covers both a finished
// recording and a warm-up that never became one (FAB drag, pointercancel, a
// press that lands elsewhere) — either way the OS recording indicator must not
// stay lit forever. releaseMicIfIdle is what defends the ordering in
// startCapture, where the sink is attached after warmMic() has already armed
// the timer.
function armIdle(): void {
	clearIdle();
	if (!isMicWarm()) return;
	idleTimer = setTimeout(releaseMicIfIdle, micIdle.releaseMs);
}

const resampleEmit = (onChunk: PcmSink) => {
	if (pendingLen === 0) return;
	const buf = new Float32Array(pendingLen);
	let off = 0;
	for (const p of pending) { buf.set(p, off); off += p.length; }
	pending = [];
	pendingLen = 0;

	const ratio = inRate / TARGET_RATE;
	const out: number[] = [];
	while (readPos < buf.length - 1) {
		const i = Math.floor(readPos);
		const frac = readPos - i;
		const s = buf[i]! * (1 - frac) + buf[i + 1]! * frac;
		out.push(Math.max(-32768, Math.min(32767, Math.round(s * 32767))));
		readPos += ratio;
	}
	readPos -= buf.length; // carry the fractional remainder into the next buffer

	for (let i = 0; i < out.length; i += samplesPerChunk) {
		onChunk(Int16Array.from(out.slice(i, i + samplesPerChunk)));
	}
};

// A warm-but-idle pipeline must accumulate nothing, or the next recording would
// open with stale pre-tap audio. While a sink IS attached this is also the only
// place that can prove the graph is alive, so it counts frames and tracks the
// first non-zero sample: a mic that returns digital silence looks exactly like
// a working one from every other vantage point.
const onFloat = (data: Float32Array) => {
	const current = sink;
	if (!current) return;
	framesSinceAttach++;
	if (peakSinceAttach === 0) {
		// Scans only until the first audible sample lands, so a live recording
		// pays this once rather than per frame.
		for (let i = 0; i < data.length; i++) {
			const a = data[i]! < 0 ? -data[i]! : data[i]!;
			if (a > peakSinceAttach) peakSinceAttach = a;
		}
	}
	pending.push(data);
	pendingLen += data.length;
	if (pendingLen >= (inRate * CHUNK_MS) / 1000) resampleEmit(current);
};

async function openPipeline(): Promise<void> {
	const myGen = generation;
	const startedAt = performance.now();
	try {
		const legacyWindow = window as typeof window & {
			webkitAudioContext?: typeof AudioContext;
		};
		const Ctx = window.AudioContext ?? legacyWindow.webkitAudioContext;
		if (!Ctx) throw new Error("Web Audio is not supported");
		ctx = new Ctx();
		const resumed = resumeContext().then(
			() => ({ ok: true }) as const,
			(error: unknown) => ({ ok: false, error }) as const,
		);
		const requestedStream = navigator.mediaDevices.getUserMedia({
			audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
		});
		stream = await requestedStream;
		const resumeResult = await resumed;
		if (!resumeResult.ok) throw resumeResult.error;
		if (generation !== myGen) { disposePipeline(); return; }
		inRate = ctx.sampleRate;
		readPos = 0;
		source = ctx.createMediaStreamSource(stream);

		// Preferred path: AudioWorklet (off the main thread). Falls back to a
		// ScriptProcessorNode when the worklet can't load (older Safari, CSP, etc.)
		// or when repairCapture() has already caught it delivering no frames on
		// this device, so capture never silently dies.
		let workletOk = false;
		if (ctx.audioWorklet && !captureQuirks.workletBroken) {
			try {
				workletUrl = URL.createObjectURL(new Blob([WORKLET_SRC], { type: "application/javascript" }));
				await ctx.audioWorklet.addModule(workletUrl);
				if (generation !== myGen) { disposePipeline(); return; }
				node = new AudioWorkletNode(ctx, "pcm-forwarder");
				node.port.onmessage = (e: MessageEvent<Float32Array>) => onFloat(e.data);
				source.connect(node);
				node.connect(ctx.destination);
				workletOk = true;
			} catch {
				workletOk = false;
			}
		}
		if (generation !== myGen) { disposePipeline(); return; }
		if (!workletOk && ctx) {
			// eslint-disable-next-line @typescript-eslint/no-deprecated
			proc = ctx.createScriptProcessor(4096, 1, 1);
			proc.onaudioprocess = (e) => onFloat(new Float32Array(e.inputBuffer.getChannelData(0)));
			source.connect(proc);
			proc.connect(ctx.destination); // required to drive onaudioprocess; output stays silent
		}
		capturePath = workletOk ? "worklet" : "scriptprocessor";

		// THE iOS BUG THIS EXISTS FOR: starting the mic switches the OS audio
		// session, which re-suspends a context that was created (and resumed)
		// BEFORE the stream existed. The graph is then fully wired to a context
		// that never renders: zero frames, zero errors, a red mic and silence —
		// measured on iOS 18.7 as ctx_state "suspended" with frames 0. So resume
		// again now that the device is live, and refuse to hand back a pipeline
		// that did not reach "running" rather than let it look healthy.
		if (ctx.state !== "running") await resumeContext().catch(() => { /* verified below */ });
		if (generation !== myGen) { disposePipeline(); return; }
		if (ctx.state !== "running") {
			throw new Error(`audio session stayed ${ctx.state} — tap the mic again`);
		}
		lastWarmupMs = performance.now() - startedAt;
	} catch (e) {
		disposePipeline();
		throw e;
	}
}

// Never hold the device in a hidden tab — but never kill a recording that is
// still running when the user switches apps on mobile.
if (typeof document !== "undefined" && typeof document.addEventListener === "function") {
	document.addEventListener("visibilitychange", () => {
		if (document.hidden) releaseMicIfIdle();
	});
}
