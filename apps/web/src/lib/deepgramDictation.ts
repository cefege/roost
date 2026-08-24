// deepgramDictation — Deepgram live STT engine. Streams linear16/16k PCM
// (audioPcmCapture) to wss://api.deepgram.com/v1/listen with interim_results
// + smart_format/punctuate. Auth: the caller injects the credential via
// grantToken (MobileVoiceInput passes the deepgramKey.ts cache) and it rides
// the ["token", key] WS subprotocol — browsers can't set WS headers.
// KeepAlive @5s, Finalize on stop, 3s wait. Emits voice.start_timing per
// recording so tap→first-caption latency stays attributable.
// Used by MobileVoiceInput when a Deepgram key is configured in coord; otherwise
// the component falls back to the built-in Web Speech recognizer.

import { createSignal, onCleanup } from "solid-js";
import {
	startCapture,
	stopCapture,
	releaseMic,
	micWarmupMs,
	captureStats,
	repairCapture,
	type CaptureStats,
} from "./audioPcmCapture.ts";
import { diag, signal } from "@roost/shared/diag";
import { buildUrl } from "./deepgramDictation.url.ts";
import {
	keytermHitRate,
	isExpectedClose,
	closeMessage,
	clientFacts,
	micOpenFailure,
} from "./deepgramDictation.helpers.ts";
export { buildUrl };

// Shared shape so MobileVoiceInput can treat Deepgram + Web Speech the same.
export interface Dictation {
	supported: boolean;
	final: () => string;
	interim: () => string;
	error: () => string | null;
	start: () => void;
	stop: () => void;
	abort: () => void;
	reset: () => void;
}

const KEEPALIVE_MS = 5000;
// One automatic reconnect on a transient drop before we surface an error —
// kills most "sometimes Deepgram connection error" (network blips, brief coord
// restart during token grant). The mic keeps running across the retry.
const RECONNECT_DELAY_MS = 500;
/** Engine deadlines. Mutable, not bare consts, so tests can shrink them (same
 *  recipe as audioPcmCapture's `micIdle` / `micTimeouts`). */
export const dictationTimings = {
	/** Wait for Deepgram's from_finalize Results after a stop. */
	finalizeWaitMs: 3_000,
	/** How long a recording may deliver ZERO audio frames before the pipeline
	 *  is rebuilt (once) and then declared dead. Long enough to cover a cold
	 *  device open on a phone, short enough that nobody stands there talking
	 *  into a mic that will never hear them. */
	silenceGraceMs: 2_500,
	/** startCapture() has not resolved by now → the device open is stalled. The
	 *  silence watch cannot cover this: it is armed FROM that resolution. */
	startGraceMs: 9_000,
};

export interface DeepgramDictationOpts {
	language: () => string; // "en" | "multi" | "__auto__"
	grantToken: () => Promise<string>;
	onEnd?: () => void;
	/** Fired ONCE per recording, the moment the mic is capturing AND the
	 *  socket is open — i.e. speech from here on can be heard. The caller uses
	 *  it to leave its "starting" UI; neither milestone alone means audible. */
	onLive?: () => void;
	// Nova-3 keyterm biasing — terms extracted from the terminal you're dictating
	// into (keytermContext.extractKeyterms). Evaluated once at WS-open (Deepgram
	// fixes keyterms per connection), so each recording snapshots fresh context.
	keyterms?: () => string[];
	/** Deepgram refused the credential — drop any cached copy before the retry. */
	onCredentialRejected?: () => void;
	/** The recording is over and it FAILED: the engine has already torn itself
	 *  down and error() holds the reason. The caller MUST leave its recording
	 *  state — a failure that leaves the UI "listening" is the silent-dead-mic
	 *  bug. Never fired for a user stop/abort. */
	onFailure?: () => void;
}

function deepgramSupported(): boolean {
	return (
		typeof window !== "undefined" &&
		"WebSocket" in window &&
		!!navigator.mediaDevices?.getUserMedia &&
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		!!(window.AudioContext ?? (window as any).webkitAudioContext)
	);
}

export function createDeepgramDictation(
	opts: DeepgramDictationOpts,
): Dictation {
	const supported = deepgramSupported();
	const [finalText, setFinalText] = createSignal("");
	const [interimText, setInterimText] = createSignal("");
	const [error, setError] = createSignal<string | null>(null);

	let ws: WebSocket | null = null;
	// Milestones for the voice.start_timing diag, all ms since the tap.
	let tapMs = 0;
	let micReadyMs = -1;
	let firstAudioMs = -1;
	let grantMs = -1;
	let wsOpenMs = -1;
	let timingEmitted = false;
	let keepAlive: ReturnType<typeof setInterval> | null = null;
	let finalizeTimer: ReturnType<typeof setTimeout> | null = null;
	let silenceTimer: ReturnType<typeof setTimeout> | null = null;
	let repairedOnce = false; // one pipeline rebuild per recording
	let chunksSent = 0;
	let bytesSent = 0;
	let resultsFrames = 0;
	let endIntent: "send" | "cancel" | null = null;
	let retried = false; // one auto-reconnect per recording (reset in start())
	let segments: string[] = [];
	let preBuffer: ArrayBuffer[] = []; // PCM captured before the WS finishes opening
	let injectedKeyterms: string[] = []; // this recording's biasing terms (for hit-rate)
	// A recording's IDENTITY. Every async continuation — the token grant, a
	// socket handler, a timer, a repair — captures it at issue and bails when it
	// no longer matches. `endIntent` cannot carry this: completeSend() resets it
	// to null, which is indistinguishable from "a recording is live".
	let runId = 0;
	// pcmSink is a stable closure that repairCapture re-attaches across
	// recordings, so it cannot capture a run token — this is its equivalent.
	let recording = false;
	let startTimer: ReturnType<typeof setTimeout> | null = null;
	let captureAttached = false;
	// The two liveness milestones onLive promises; either may land first.
	let micAttached = false;
	let socketOpen = false;
	let liveNotified = false;

	// Exactly-once per recording: both milestones in, nobody cancelled.
	const notifyLive = () => {
		if (liveNotified || endIntent !== null || !micAttached || !socketOpen) return;
		liveNotified = true;
		opts.onLive?.();
	};

	const teardown = () => {
		runId++; // every in-flight continuation of the finished run is now stale
		recording = false;
		micAttached = false;
		socketOpen = false;
		if (startTimer) {
			clearTimeout(startTimer);
			startTimer = null;
		}
		if (keepAlive) {
			clearInterval(keepAlive);
			keepAlive = null;
		}
		if (finalizeTimer) {
			clearTimeout(finalizeTimer);
			finalizeTimer = null;
		}
		if (silenceTimer) {
			clearTimeout(silenceTimer);
			silenceTimer = null;
		}
		try {
			stopCapture();
		} catch {
			/* ignore */
		}
		preBuffer = [];
		if (ws) {
			try {
				if (ws.readyState === WebSocket.OPEN)
					ws.send(JSON.stringify({ type: "CloseStream" }));
			} catch {
				/* ignore */
			}
			try {
				ws.onmessage = null;
				ws.onerror = null;
				ws.onclose = null;
				ws.close();
			} catch {
				/* ignore */
			}
			ws = null;
		}
	};

	// Terminal failure. endIntent="cancel" makes the WS close that follows an
	// EXPECTED one (isExpectedClose), so a second message can't stack on the
	// first, and teardown() does NOT clear error() — the reason survives for
	// the caption.
	const fail = () => {
		if (endIntent === "cancel") return;
		endIntent = "cancel";
		teardown();
		opts.onFailure?.();
	};

	const completeSend = () => {
		if (endIntent !== "send") return;
		endIntent = null;
		const stats = captureStats();
		const transcript = segments.join(" ");
		if (injectedKeyterms.length > 0) {
			diag("voice.keyterm_hits", {
				injected: injectedKeyterms.length,
				hit_rate: Number(
					keytermHitRate(injectedKeyterms, transcript).toFixed(2),
				),
				chars: transcript.length,
			});
		}
		// A recording that ends with nothing to show IS the reported bug, and it
		// reaches here without a single error anywhere. One always-on line says
		// which stage went quiet — no frames (dead graph), frames but peak 0
		// (the device handed us silence), chunks but no Results (Deepgram never
		// answered), or Results with no text (it heard nothing) — because that
		// is not recoverable from any other vantage point once the tab is gone.
		if (transcript.trim().length === 0) {
			signal("voice.dictation_empty", {
				...captureFacts(stats),
				chunks: chunksSent,
				bytes: bytesSent,
				results: resultsFrames,
				first_audio_ms: Math.round(firstAudioMs),
				ws_open_ms: Math.round(wsOpenMs),
				repaired: repairedOnce,
				...clientFacts(),
			});
		}
		teardown();
		opts.onEnd?.();
	};

	const captureFacts = (s: CaptureStats) => ({
		frames: s.frames,
		peak: Number(s.peak.toFixed(4)),
		path: s.path,
		ctx_state: s.ctxState,
		sample_rate: s.sampleRate,
	});

	// A recording can be attached, warm, and streaming NOTHING: on a phone the
	// AudioContext gets suspended/"interrupted" out from under it, or the
	// AudioWorklet ships dead (see audioPcmCapture liveness). Deepgram simply
	// stays quiet, so the engine has to check delivery itself — rebuild the
	// pipeline once, then end the recording with a reason instead of listening
	// forever. Only ZERO frames triggers this: a real mic in a quiet room can
	// legitimately report peak 0 for seconds, and killing that would be worse
	// than the bug.
	const armSilenceWatch = (run: number) => {
		if (silenceTimer) {
			clearTimeout(silenceTimer);
			silenceTimer = null;
		}
		silenceTimer = setTimeout(() => {
			silenceTimer = null;
			if (run !== runId || endIntent !== null) return;
			const s = captureStats();
			if (s.frames > 0) return;
			if (repairedOnce) {
				giveUpSilent(s);
				return;
			}
			repairedOnce = true;
			signal("voice.mic_failed", {
				stage: "silent_retry",
				...captureFacts(s),
				...clientFacts(),
				cooldownKey: "voice",
			});
			repairCapture(pcmSink)
				.then((attached) => {
					if (run !== runId || endIntent !== null) return;
					if (!attached) {
						giveUpSilent(captureStats());
						return;
					}
					armSilenceWatch(run);
				})
				.catch(() => {
					if (run === runId && endIntent === null) giveUpSilent(captureStats());
				});
		}, dictationTimings.silenceGraceMs);
	};

	const giveUpSilent = (s: CaptureStats) => {
		signal("voice.mic_failed", {
			stage: "silent",
			...captureFacts(s),
			...clientFacts(),
			cooldownKey: "voice",
		});
		setError(
			"Mic opened but sent no audio — tap the mic again (iOS only starts audio inside a fresh tap). If it repeats, close any other app using the mic.",
		);
		fail();
	};

	// Named so a repair can re-attach the SAME sink to the rebuilt pipeline.
	const pcmSink = (pcm16: Int16Array) => {
		if (!recording) return;
		if (firstAudioMs < 0) firstAudioMs = performance.now() - tapMs;
		chunksSent++;
		bytesSent += pcm16.byteLength;
		if (ws && ws.readyState === WebSocket.OPEN) {
			try {
				ws.send(pcm16.buffer as ArrayBuffer);
			} catch {
				/* ignore */
			}
		} else {
			preBuffer.push(pcm16.buffer as ArrayBuffer);
		}
	};

	// One line per recording, ms since the tap; -1 for a milestone that never
	// happened. warm_ms is 0 on a reused pipeline and the cold-open cost
	// otherwise — the field that says whether the mic was actually warm.
	const emitTiming = () => {
		if (timingEmitted) return;
		timingEmitted = true;
		diag("voice.start_timing", {
			warm_ms: Math.round(micWarmupMs()),
			mic_ms: Math.round(micReadyMs),
			first_audio_ms: Math.round(firstAudioMs),
			grant_ms: Math.round(grantMs),
			ws_open_ms: Math.round(wsOpenMs),
			first_text_ms: Math.round(performance.now() - tapMs),
		});
	};

	const start = () => {
		const run = ++runId;
		recording = true;
		setError(null);
		endIntent = null;
		retried = false;
		segments = [];
		preBuffer = [];
		repairedOnce = false;
		chunksSent = 0;
		bytesSent = 0;
		resultsFrames = 0;
		micAttached = false;
		socketOpen = false;
		liveNotified = false;
		setFinalText("");
		setInterimText("");
		tapMs = performance.now();
		micReadyMs = -1;
		firstAudioMs = -1;
		grantMs = -1;
		wsOpenMs = -1;
		timingEmitted = false;
		captureAttached = false;

		// A recording whose capture never comes UP must end with a reason. The
		// silence watch is armed from startCapture's resolution and therefore
		// cannot see the failure where startCapture never resolves at all — the
		// measured iOS PWA case (path "none", ctx_state "suspended", repaired
		// false, for the whole recording).
		startTimer = setTimeout(() => {
			startTimer = null;
			if (run !== runId || endIntent !== null || captureAttached) return;
			signal("voice.mic_failed", {
				stage: "start_stalled",
				...captureFacts(captureStats()),
				...clientFacts(),
				cooldownKey: "voice",
			});
			// The singleton is what stalled; drop it so the NEXT tap opens a
			// fresh device instead of awaiting the same dead promise.
			releaseMic();
			setError("Mic didn't open — tap the mic again.");
			fail();
		}, dictationTimings.startGraceMs);

		// Start the mic NOW, synchronously inside the tap gesture. Safari (and
		// others) only grant getUserMedia within the user-gesture window — calling
		// it later (after the token grant + WS handshake) is denied without even
		// prompting. Audio captured before the WS opens is buffered, then flushed.
		startCapture(pcmSink)
			.then((attached) => {
				if (run !== runId) return;
				if (attached) {
					captureAttached = true;
					micReadyMs = performance.now() - tapMs;
					micAttached = true;
					notifyLive();
					// Attached is not the same as ALIVE — see armSilenceWatch.
					armSilenceWatch(run);
					return;
				}
				if (endIntent !== null) return; // the user stopped/cancelled first
				signal("voice.mic_failed", { stage: "attach", cooldownKey: "voice" });
				setError("Mic didn't start — tap the mic and try again.");
				fail();
			})
			.catch((e: unknown) => {
				if (run !== runId) return;
				const f = micOpenFailure(e);
				setError(f.message);
				signal("voice.mic_failed", {
					stage: "open",
					name: f.name,
					detail: f.detail,
					cooldownKey: "voice",
				});
				fail();
			});

		void connectWs(run);
	};

	// Open (or RE-open, on a transient drop) the Deepgram socket. The mic keeps
	// running across a reconnect, buffering into preBuffer, so no speech is lost.
	const connectWs = async (run: number) => {
		let token: string;
		try {
			token = await opts.grantToken();
			grantMs = performance.now() - tapMs;
		} catch {
			// Token grant can fail transiently (coord restarting) — reconnect once.
			if (!retried && run === runId) {
				retried = true;
				diag("voice.ws_retry", { stage: "grant" });
				setTimeout(() => {
					void connectWs(run);
				}, RECONNECT_DELAY_MS);
				return;
			}
			signal("voice.ws_failed", { stage: "grant_giveup", cooldownKey: "voice" });
			setError(
				"Voice service unavailable — couldn't reach Deepgram (coordinator may be restarting). Try again.",
			);
			fail();
			return;
		}
		if (run !== runId) return; // this recording already ended
		let keyterms: string[] = [];
		// KTF4 — was a silent catch; a throwing extractor on a new harness would
		// vanish. Signal it (always-on) so the next regression is in *.err.log.
		try {
			keyterms = opts.keyterms?.() ?? [];
		} catch (e) {
			signal("voice.ws_failed", {
				stage: "keyterms",
				detail: String(e instanceof Error ? e.message : e),
			});
		}
		injectedKeyterms = keyterms;
		const url = buildUrl(opts.language(), keyterms);
		// KTF4 — url_bytes is the diagnostic that would have caught this class:
		// a handshake reset with no Error frame correlates with a bloated URL.
		diag("voice.keyterms", {
			count: keyterms.length,
			url_bytes: url.length,
			sample: keyterms.slice(0, 8),
		});
		// Raw API key → "token" subprotocol (a minted access token would use "bearer").
		ws = new WebSocket(url, ["token", token]);
		ws.binaryType = "arraybuffer";

		ws.onopen = () => {
			if (run !== runId) return;
			wsOpenMs = performance.now() - tapMs;
			socketOpen = true;
			notifyLive();
			keepAlive = setInterval(() => {
				try {
					ws?.send(JSON.stringify({ type: "KeepAlive" }));
				} catch {
					/* ignore */
				}
			}, KEEPALIVE_MS);
			// Flush whatever the mic captured during the grant + handshake.
			for (const buf of preBuffer) {
				try {
					ws?.send(buf);
				} catch {
					/* ignore */
				}
			}
			preBuffer = [];
		};

		ws.onmessage = (ev) => {
			if (run !== runId) return;
			// eslint-disable-next-line @typescript-eslint/no-explicit-any
			let msg: any;
			try {
				msg = JSON.parse(typeof ev.data === "string" ? ev.data : "");
			} catch {
				diag("voice.frame_parse_failed", {});
				return;
			}
			// Deepgram reports bad params (e.g. "Keyterm limit exceeded. The
			// maximum number of tokens across all keyterms is 500.") as an Error
			// MESSAGE, not a close frame. Always-on signal so the next occurrence
			// is diagnosable from *.err.log instead of a vanished toast.
			if (msg?.type === "Error" || msg?.err_msg) {
				opts.onCredentialRejected?.();
				const detail =
					msg.err_msg ?? msg.description ?? msg.message ?? "unknown";
				signal("voice.ws_failed", {
					stage: "msg",
					err_code: msg.err_code,
					detail,
					keyterms: injectedKeyterms.length,
				});
				if (!error()) setError(`Deepgram rejected the request: ${detail}`);
				fail();
				return;
			}
			if (msg?.type !== "Results") return;
			resultsFrames++;
			const transcript: string =
				msg.channel?.alternatives?.[0]?.transcript ?? "";
			if (transcript.trim()) emitTiming();
			if (msg.is_final) {
				if (transcript.trim()) {
					segments.push(transcript);
					setFinalText(segments.join(" "));
				}
				setInterimText("");
				if (msg.from_finalize) completeSend();
			} else {
				setInterimText(transcript);
			}
		};

		// onerror always precedes onclose for a WS — let onclose own the
		// retry/error decision so we don't short-circuit the reconnect.
		ws.onerror = () => {
			/* handled in onclose */
		};
		ws.onclose = (e) => {
			if (run !== runId) return;
			if (keepAlive) {
				clearInterval(keepAlive);
				keepAlive = null;
			}
			if (!isExpectedClose(e.code, endIntent)) {
				// 4xxx is the range closeMessage classifies as "API key may be
				// invalid" — drop the cached credential before the retry re-grants.
				if (e.code >= 4000) opts.onCredentialRejected?.();
				// Unexpected drop while listening → reconnect ONCE, mic still running.
				if (!retried) {
					retried = true;
					diag("voice.ws_retry", { stage: "ws", code: e.code });
					setTimeout(() => {
						void connectWs(run);
					}, RECONNECT_DELAY_MS);
					return;
				}
				signal("voice.ws_failed", {
					stage: "close",
					code: e.code,
					reason: e.reason,
					keyterms: injectedKeyterms.length,
				});
				if (!error()) setError(closeMessage(e.code, e.reason));
				fail();
			}
			if (endIntent === "send") completeSend();
		};
	};

	const stop = () => {
		endIntent = "send";
		emitTiming(); // a silent recording still reports where the time went
		try {
			stopCapture();
		} catch {
			/* ignore */
		}
		if (ws?.readyState === WebSocket.OPEN) {
			try {
				ws.send(JSON.stringify({ type: "Finalize" }));
			} catch {
				/* ignore */
			}
			finalizeTimer = setTimeout(completeSend, dictationTimings.finalizeWaitMs);
		} else {
			// WS never opened (grant/connect in flight or failed) — end with whatever we have.
			completeSend();
		}
	};

	const abort = () => {
		endIntent = "cancel";
		setInterimText("");
		teardown();
	};

	const reset = () => {
		segments = [];
		setFinalText("");
		setInterimText("");
		setError(null);
	};

	onCleanup(() => {
		endIntent = "cancel";
		teardown();
	});

	return {
		supported,
		final: finalText,
		interim: interimText,
		error,
		start,
		stop,
		abort,
		reset,
	};
}
