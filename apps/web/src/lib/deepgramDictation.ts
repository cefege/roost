// deepgramDictation — Deepgram live STT engine. Streams linear16/16k PCM
// (audioPcmCapture) to wss://api.deepgram.com/v1/listen with interim_results
// + smart_format/punctuate. Auth: the SPA never holds the key — coord grants a
// ~30s token and the browser passes it via the ["bearer", token] WS subprotocol
// (browsers can't set WS headers). KeepAlive @5s, Finalize on stop, 3s wait.
// Used by MobileVoiceInput when a Deepgram key is configured in coord; otherwise
// the component falls back to the built-in Web Speech recognizer.

import { createSignal, onCleanup } from "solid-js";
import { createPcmCapture, type PcmCapture } from "./audioPcmCapture.ts";
import { diag, signal } from "@roost/shared/diag";
import { buildUrl } from "./deepgramDictation.url.ts";
import { keytermHitRate, isExpectedClose, closeMessage } from "./deepgramDictation.helpers.ts";
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

const FINALIZE_WAIT_MS = 3000;
const KEEPALIVE_MS = 5000;
// One automatic reconnect on a transient drop before we surface an error —
// kills most "sometimes Deepgram connection error" (network blips, brief coord
// restart during token grant). The mic keeps running across the retry.
const RECONNECT_DELAY_MS = 500;

export interface DeepgramDictationOpts {
	language: () => string; // "en" | "multi" | "__auto__"
	grantToken: () => Promise<{ accessToken: string; expiresIn: number }>;
	onEnd?: () => void;
	// Nova-3 keyterm biasing — terms extracted from the terminal you're dictating
	// into (keytermContext.extractKeyterms). Evaluated once at WS-open (Deepgram
	// fixes keyterms per connection), so each recording snapshots fresh context.
	keyterms?: () => string[];
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
	let capture: PcmCapture | null = null;
	let keepAlive: ReturnType<typeof setInterval> | null = null;
	let finalizeTimer: ReturnType<typeof setTimeout> | null = null;
	let endIntent: "send" | "cancel" | null = null;
	let retried = false; // one auto-reconnect per recording (reset in start())
	let segments: string[] = [];
	let preBuffer: ArrayBuffer[] = []; // PCM captured before the WS finishes opening
	let injectedKeyterms: string[] = []; // this recording's biasing terms (for hit-rate)

	const teardown = () => {
		if (keepAlive) {
			clearInterval(keepAlive);
			keepAlive = null;
		}
		if (finalizeTimer) {
			clearTimeout(finalizeTimer);
			finalizeTimer = null;
		}
		try {
			capture?.stop();
		} catch {
			/* ignore */
		}
		capture = null;
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

	const completeSend = () => {
		if (endIntent !== "send") return;
		endIntent = null;
		if (injectedKeyterms.length > 0) {
			const transcript = segments.join(" ");
			diag("voice.keyterm_hits", {
				injected: injectedKeyterms.length,
				hit_rate: Number(
					keytermHitRate(injectedKeyterms, transcript).toFixed(2),
				),
				chars: transcript.length,
			});
		}
		teardown();
		opts.onEnd?.();
	};

	const start = () => {
		setError(null);
		endIntent = null;
		retried = false;
		segments = [];
		preBuffer = [];
		setFinalText("");
		setInterimText("");

		// Start the mic NOW, synchronously inside the tap gesture. Safari (and
		// others) only grant getUserMedia within the user-gesture window — calling
		// it later (after the token grant + WS handshake) is denied without even
		// prompting. Audio captured before the WS opens is buffered, then flushed.
		capture = createPcmCapture();
		capture
			.start((pcm16) => {
				if (ws && ws.readyState === WebSocket.OPEN) {
					try {
						ws.send(pcm16.buffer as ArrayBuffer);
					} catch {
						/* ignore */
					}
				} else {
					preBuffer.push(pcm16.buffer as ArrayBuffer);
				}
				// eslint-disable-next-line @typescript-eslint/no-explicit-any
			})
			.catch((e: any) => {
				const name = e?.name || "";
				const msg = e?.message || "";
				if (name === "NotAllowedError" || /denied|permission/i.test(msg)) {
					setError(
						"Mic blocked for this site — allow it (address-bar icon → Microphone → Allow), then reload.",
					);
				} else if (
					name === "NotReadableError" ||
					name === "AbortError" ||
					/in use|busy|could not start|failed to allocate/i.test(msg)
				) {
					// Another tab/app holds the mic (WhatsApp/Telegram/Zoom/Meet) — macOS
					// hands the device to one page at a time. This is the #1 intermittent
					// "mic error" cause; name it so it's actionable.
					setError(
						"Mic is busy — another tab or app (WhatsApp, Telegram, Zoom…) is using it. Close it, then retry.",
					);
				} else if (name === "NotFoundError") {
					setError("No microphone found on this device.");
				} else {
					setError("Mic: " + (msg || name || "unavailable"));
				}
			});

		void connectWs();
	};

	// Open (or RE-open, on a transient drop) the Deepgram socket. The mic keeps
	// running across a reconnect, buffering into preBuffer, so no speech is lost.
	const connectWs = async () => {
		let token: string;
		try {
			token = (await opts.grantToken()).accessToken;
		} catch {
			// Token grant can fail transiently (coord restarting) — reconnect once.
			if (!retried && endIntent !== "cancel") {
				retried = true;
				diag("voice.ws_retry", { stage: "grant" });
				setTimeout(() => {
					void connectWs();
				}, RECONNECT_DELAY_MS);
				return;
			}
			signal("voice.ws_failed", { stage: "grant_giveup", cooldownKey: "voice" });
			setError(
				"Voice service unavailable — couldn't reach Deepgram (coordinator may be restarting). Try again.",
			);
			return;
		}
		if (endIntent === "cancel") return; // aborted during grant
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
				const detail =
					msg.err_msg ?? msg.description ?? msg.message ?? "unknown";
				signal("voice.ws_failed", {
					stage: "msg",
					err_code: msg.err_code,
					detail,
					keyterms: injectedKeyterms.length,
				});
				if (!error()) setError(`Deepgram rejected the request: ${detail}`);
				return;
			}
			if (msg?.type !== "Results") return;
			const transcript: string =
				msg.channel?.alternatives?.[0]?.transcript ?? "";
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
			if (keepAlive) {
				clearInterval(keepAlive);
				keepAlive = null;
			}
			if (!isExpectedClose(e.code, endIntent)) {
				// Unexpected drop while listening → reconnect ONCE, mic still running.
				if (!retried) {
					retried = true;
					diag("voice.ws_retry", { stage: "ws", code: e.code });
					setTimeout(() => {
						void connectWs();
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
			}
			if (endIntent === "send") completeSend();
		};
	};

	const stop = () => {
		endIntent = "send";
		try {
			capture?.stop();
		} catch {
			/* ignore */
		}
		capture = null;
		if (ws?.readyState === WebSocket.OPEN) {
			try {
				ws.send(JSON.stringify({ type: "Finalize" }));
			} catch {
				/* ignore */
			}
			finalizeTimer = setTimeout(completeSend, FINALIZE_WAIT_MS);
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
