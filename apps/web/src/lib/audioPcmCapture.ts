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

// Linear-interpolation resampler state (inputRate → 16 kHz). Fractional read
// position carries across chunks so chunk boundaries don't drop/dup samples.
let inRate = 48000;
let readPos = 0;
let pending: Float32Array[] = [];
let pendingLen = 0;
const samplesPerChunk = Math.round((TARGET_RATE * CHUNK_MS) / 1000); // 640

// ── exported API ──────────────────────────────────────────────────────────

/** Opens the mic + AudioContext + worklet, or resolves immediately when the
 *  pipeline is already warm. Idempotent; concurrent calls share one in-flight
 *  promise. Rejects with the raw getUserMedia error. */
export function warmMic(): Promise<void> {
	clearIdle();
	if (isMicWarm()) {
		lastWarmupMs = 0;
		return resumeContext().finally(armIdle);
	}
	if (warming) return warming;
	warming = openPipeline().finally(() => {
		warming = null;
		armIdle();
	});
	return warming;
}

/** Routes captured PCM to `nextSink`, warming the pipeline first when cold.
 *  Resets the resampler so each recording starts clean. */
export async function startCapture(nextSink: PcmSink): Promise<void> {
	const epoch = ++captureEpoch;
	await warmMic();
	// A releaseMic() that landed *before* this call but during an earlier
	// warm-up cancelled the pipeline we just awaited: the promise resolves with
	// nothing open. Re-open once rather than leave a silently dead mic. (A
	// release AFTER this call bumped captureEpoch and is honoured below.)
	if (epoch === captureEpoch && !isMicWarm()) await warmMic();
	// Stopped or released while warming — attaching now would stream a finished
	// recording's audio into a dead sink.
	if (epoch !== captureEpoch || !isMicWarm()) return;
	readPos = 0;
	pending = [];
	pendingLen = 0;
	sink = nextSink;
	clearIdle(); // a live recording is never on the release clock
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

// Disposal WITHOUT the cancellation bumps: a warm-up that discovers it was
// cancelled must drop its half-built pipeline without also invalidating a
// startCapture that arrived after the cancellation and is waiting to re-open.
function disposePipeline(): void {
	sink = null;
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
// stay lit forever. The guard defends the ordering in startCapture, where the
// sink is attached after warmMic() has already armed the timer.
function armIdle(): void {
	clearIdle();
	if (!isMicWarm()) return;
	idleTimer = setTimeout(() => {
		if (!sink) releaseMic();
	}, micIdle.releaseMs);
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
// open with stale pre-tap audio.
const onFloat = (data: Float32Array) => {
	const current = sink;
	if (!current) return;
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
		// so capture never silently dies.
		let workletOk = false;
		if (ctx.audioWorklet) {
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
		if (document.hidden && !sink) releaseMic();
	});
}
