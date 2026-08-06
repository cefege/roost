// MobileVoiceInput — mic button + live transcript.
// State: idle → listening → finalizing → (deliver). Tap to start; tap again to
// stop, which finalizes and delivers automatically — no review step. Where the
// text lands depends on the caller: without onTranscript it is submitted to the
// PTY (the corner FAB), with onTranscript it is inserted into the caller's draft
// (the composer bar) and the caller's own send commits it. While recording (or
// finalizing) an ✕ button discards without delivering.
//
// Engine: if a Deepgram key is configured in coord, dictation streams to
// Deepgram (deepgramDictation); otherwise it uses the browser's built-in Web
// Speech recognizer. The tap/finalize/deliver UI is identical for both.
// Callers: CellTerminal.tsx (corner FAB) and TerminalComposeButton.tsx
// (variant="inline" inside the composer bar, where onTranscript redirects the
// finalized text into the draft instead of auto-sending).

import type { Component } from "solid-js";
import {
	Show,
	createEffect,
	createSignal,
	onCleanup,
	onMount,
} from "solid-js";
import type { ChannelId } from "@roost/shared/wire";
import { coordClient } from "../connect.ts";
import { createDeepgramDictation } from "../lib/deepgramDictation.ts";
import {
	getDeepgramKey,
	invalidateDeepgramKey,
	prefetchDeepgramKey,
} from "../lib/deepgramKey.ts";
import { micIdle, warmMic } from "../lib/audioPcmCapture.ts";
import {
	buildAccum,
	finalizeKeyterms,
	type TerminalContext,
} from "../lib/keytermContext.ts";
import { learnTerms, lexiconTopTerms } from "../lib/keytermLexicon.ts";
import { keytermBiasing } from "../lib/keytermBiasingPref.ts";
import { onFabPointerDown } from "../lib/fabDragOffset.ts";
import { isTouchDevice } from "../lib/windowSizeClass.ts";

// The capture pipeline stays warm after a recording so the next tap skips a
// 1–2 s cold device open. On a PHONE that window is also how long iOS/Android
// keep their recording indicator lit — a minute of orange dot after every
// dictation reads as "this app is still listening", and it hands iOS a whole
// minute in which to suspend the AudioContext under an idle-but-open pipeline.
// Long enough to chain a stop-and-retap, short enough that the indicator clears
// with the recording. Desktop keeps the long hold: no indicator, and the cold
// open is the only thing that ever ate the first spoken word.
if (isTouchDevice()) micIdle.releaseMs = 4_000;

// ─── shared recording state ───────────────────────────────────────────────
// All MobileVoiceInput instances share this signal. Only one can record at a
// time — the owning instance renders; all others return null early. This
// prevents multiple position:fixed elements from overlapping at the same
// viewport coordinates when multiple terminals are visible.
export const [activeVoiceChannel, setActiveVoiceChannel] = createSignal<number | null>(null);

// Transcription config is global, not per-pane. Cached at module scope because
// the composer's inline mic remounts on every composer open — a per-mount RPC
// would race the first tap into the Web Speech fallback.
type TranscriptionConfig = { deepgramConfigured: boolean; deepgramLanguage: string };
const [transcriptionConfig, setTranscriptionConfig] = createSignal<TranscriptionConfig | null>(null);
let configFetch: Promise<unknown> | null = null;
function ensureTranscriptionConfig(): void {
	configFetch ??= coordClient
		.transcriptionGetConfig({})
		.then((c) => setTranscriptionConfig({ deepgramConfigured: c.deepgramConfigured, deepgramLanguage: c.deepgramLanguage }))
		.catch(() => {
			configFetch = null;
		});
}

// ─── voice recognition shim (built-in fallback) ──────────────────────────

interface SpeechRecognitionEvent extends Event {
	resultIndex: number;
	results: SpeechRecognitionResultList;
}
interface SpeechRecognitionResultList {
	length: number;
	item(index: number): SpeechRecognitionResult;
	[index: number]: SpeechRecognitionResult;
}
interface SpeechRecognitionResult {
	isFinal: boolean;
	length: number;
	item(index: number): SpeechRecognitionAlternative;
	[index: number]: SpeechRecognitionAlternative;
}
interface SpeechRecognitionAlternative {
	transcript: string;
	confidence: number;
}
interface SpeechRecognitionErrorEvent extends Event {
	error: string;
	message: string;
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnySpeechRecognition = any;

function isWebSpeechSupported(): boolean {
	return (
		typeof window !== "undefined" &&
		("SpeechRecognition" in window || "webkitSpeechRecognition" in window)
	);
}

function createSpeechRecognition(): AnySpeechRecognition {
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	const SR =
		(window as any).SpeechRecognition ??
		(window as any).webkitSpeechRecognition;
	const r = new SR() as AnySpeechRecognition;
	r.continuous = true;
	r.interimResults = true;
	r.lang = "en-US";
	return r;
}

// ─── types ───────────────────────────────────────────────────────────────

// listening = mic recording; finalizing = stopped, waiting for the engine to
// settle the last words before the auto-send fires.
type VoiceState = "idle" | "listening" | "finalizing";

interface Props {
	channelId: ChannelId;
	/** Submits finalized dictation through the pane's current terminal mode. */
	onTerminalSubmit: (text: string) => void;

	// bias Deepgram toward on-screen jargon (keytermContext). Deepgram-only.
	readContext?: () => TerminalContext;
	// Re-grab the hidden wterm textarea after a send/discard. The mic <button>
	// holds DOM focus while recording; auto-send fires from an engine callback
	// (no click/focus event) so nothing re-focuses the terminal → the next Enter
	// hits a global handler (jumps workspace) instead of the PTY. CellTerminal
	// passes term.forceFocus() here.
	refocusTerminal?: () => void;
	/** When set, a finalized transcript goes HERE instead of the PTY (chat
	 *  composer): the composer owns the draft and its own send path. */
	onTranscript?: (text: string) => void;
	/** When set, the caller paints the transcript itself (the composer streams it
	 *  into its text field), so this component renders NO transcript caption —
	 *  two boxes for one utterance is just noise. `text` is the whole transcript
	 *  so far and REPLACES the previous value; `null` means the dictation ended
	 *  without committing (discard) and the caller should drop what it showed. */
	onLiveTranscript?: (text: string | null) => void;
	/** "inline" drops the position:fixed FAB dock so the mic can sit in a row. */
	variant?: "fab" | "inline";
}

// ─── component ───────────────────────────────────────────────────────────

export const MobileVoiceInput: Component<Props> = (props) => {
	const [voiceState, setVoiceState] = createSignal<VoiceState>("idle");
	const [interimText, setInterimText] = createSignal("");
	const [finalText, setFinalText] = createSignal("");
	const [errorMsg, setErrorMsg] = createSignal<string | null>(null);
	// If another session owns the recording, hide completely. Only ONE MobileVoiceInput
	// DOM tree exists at a time — no position:fixed overlap when multiple terminals are visible.
	const owner = activeVoiceChannel();
	if (owner !== null && owner !== props.channelId) return null;

	// Deepgram config from coord (which engine + language).
	ensureTranscriptionConfig();
	const dg = createDeepgramDictation({
		language: () => transcriptionConfig()?.deepgramLanguage ?? "en",
		grantToken: () => getDeepgramKey(),
		onCredentialRejected: invalidateDeepgramKey,
		// Engine finished finalizing after a stop() → send whatever settled.
		onEnd: () => sendTranscript(),
		// A failed recording must leave the recording state (see failToIdle).
		onFailure: () => failToIdle(),
		// Snapshot terminal context → keyterms at WS-open (best-effort). Learn from
		// the PURE-live extraction (no lexicon) so recurring seeds still decay; then
		// seed the persisted lexicon into what Deepgram actually gets.
		keyterms: () => {
			if (!keytermBiasing()) return []; // A/B toggle — Settings → Voice
			if (!props.readContext) return lexiconTopTerms(40);
			// Tokenize the live context ONCE, then finalize twice: learn the seed-free
			// set (no lexicon runaway), seed the lexicon into what Deepgram receives.
			const acc = buildAccum(props.readContext());
			learnTerms(finalizeKeyterms(acc));
			return finalizeKeyterms(acc, lexiconTopTerms(40));
		},
	});

	const useDeepgram = () => !!transcriptionConfig()?.deepgramConfigured && dg.supported;
	const webSupported = isWebSpeechSupported();
	const engineAvailable = webSupported || dg.supported;

	// Unified getters so the JSX is engine-blind.
	const dispFinal = () => (useDeepgram() ? dg.final() : finalText());
	const dispInterim = () => (useDeepgram() ? dg.interim() : interimText());
	const dispError = () => (useDeepgram() ? dg.error() : errorMsg());

	// Live feed for a caller that paints the transcript itself. Fires on every
	// engine update while a dictation is open; the idle guard keeps the post-reset
	// clear (dg.reset() empties both getters) from wiping the caller's text.
	createEffect(() => {
		const live = `${dispFinal()} ${dispInterim()}`.trim();
		if (voiceState() === "idle") return;
		props.onLiveTranscript?.(live);
	});

	let recognition: AnySpeechRecognition | null = null;
	// Web Speech `onend` fires for BOTH stop()-to-send and abort()-to-discard;
	// this flag tells them apart (Deepgram tracks its own endIntent internally).
	let webIntent: "send" | "cancel" | null = null;

	onMount(() => {
		if (!webSupported) return;
		recognition = createSpeechRecognition();
		recognition.onresult = (e: SpeechRecognitionEvent) => {
			let interim = "";
			let final = finalText();
			for (let i = e.resultIndex; i < e.results.length; i++) {
				const res = e.results[i]!;
				if (res.isFinal) final += res[0]!.transcript;
				else interim += res[0]!.transcript;
			}
			setFinalText(final);
			setInterimText(interim);
		};
		recognition.onend = () => {
			if (webIntent === "cancel") {
				webIntent = null;
				return;
			}
			webIntent = null;
			sendTranscript(); // handles empty → idle, otherwise sends
		};
		recognition.onerror = (e: SpeechRecognitionErrorEvent) => {
			setErrorMsg(e.error ?? "recognition error");
			props.onLiveTranscript?.(null);
			setVoiceState("idle");
			setActiveVoiceChannel(null);
			setTimeout(() => setErrorMsg(null), 3000);
		};
	});

	onCleanup(() => {
		if (activeVoiceChannel() === props.channelId) setActiveVoiceChannel(null);
		try {
			recognition?.abort();
		} catch {
			/* ignore */
		}
		recognition = null;
	});

	const startRecording = () => {
		setVoiceState("listening");
		// Claim the recording slot — all other terminals' MobileVoiceInput instances hide.
		setActiveVoiceChannel(props.channelId);
		if (useDeepgram()) {
			dg.reset();
			dg.start();
		} else {
			webIntent = null;
			setFinalText("");
			setInterimText("");
			try {
				recognition?.start();
			} catch {
				/* already running */
			}
		}
	};

	// Stop recording and send automatically. Moves to `finalizing`; the actual
	// send fires from the engine's onEnd (Deepgram) / onend (Web Speech).
	const stopAndSend = () => {
		setVoiceState("finalizing");
		if (useDeepgram()) {
			dg.stop();
		} else {
			webIntent = "send";
			try {
				recognition?.stop();
			} catch {
				/* ignore */
			}
		}
	};

	const toggleRecord = () => {
		if (!engineAvailable) {
			setErrorMsg(
				typeof window !== "undefined" && !window.isSecureContext
					? "Dictation needs an https connection to this page."
					: "This browser has no microphone input.",
			);
			return;
		}
		setErrorMsg(null);
		const s = voiceState();
		if (s === "idle") startRecording();
		else if (s === "listening") stopAndSend();
		// finalizing: ignore taps until the send completes.
	};

	// Reset the transcript + state back to idle and hand focus back to the
	// terminal. Shared by auto-send (empty + after-send) and discard.
	const resetToIdle = () => {
		dg.reset();
		setFinalText("");
		setInterimText("");
		setVoiceState("idle");
		setActiveVoiceChannel(null);
		// Release the recording slot — other terminals' mic buttons reappear.
		// Only refocus if focus never left the recording UI. If the user clicked
		// another terminal during recording, leave focus there.
		const voiceEl = document.querySelector('[data-testid="mobile-voice-input"]');
		if (!voiceEl || voiceEl.contains(document.activeElement)) {
			props.refocusTerminal?.();
		}
	};

	// A failed recording must LEAVE the recording state — the engine has already
	// torn itself down and its error() is what the caption shows, so nothing here
	// may reset it. Releasing the voice slot matters as much as the state: while
	// it is held, every OTHER pane's MobileVoiceInput early-returns null and the
	// mic disappears from their composers.
	const failToIdle = () => {
		if (voiceState() === "idle") return;
		props.onLiveTranscript?.(null);
		setFinalText("");
		setInterimText("");
		setVoiceState("idle");
		setActiveVoiceChannel(null);
	};

	// Called from the engine's end callback (auto-send) — NOT a button anymore.
	const sendTranscript = () => {
		const text = `${dispFinal()} ${dispInterim()}`.trim();
		if (text.length === 0) {
			resetToIdle();
			return;
		}
		if (props.onTranscript) {
			props.onTranscript(text);
			resetToIdle();
			return;
		}
		props.onTerminalSubmit(text);
		resetToIdle();
	};

	// ✕ during recording/finalizing — discard without sending.
	const discard = () => {
		if (useDeepgram()) {
			dg.abort();
		} else {
			webIntent = "cancel";
			try {
				recognition?.abort();
			} catch {
				/* ignore */
			}
		}
		// The caller painted this dictation; tell it to drop what it showed.
		props.onLiveTranscript?.(null);
		resetToIdle();
	};

	const isActive = () => voiceState() !== "idle";
	// An error still needs a surface of its own — it must never land in the
	// caller's draft, where send would type it into the PTY. The TRANSCRIPT
	// caption is what a live-feed caller replaces.
	const showCaption = () => !!dispError() || (isActive() && !props.onLiveTranscript);
	// With onTranscript set (the composer's inline mic) stopping INSERTS into the
	// draft — it never submits. A "send" glyph would sit right beside the bar's
	// real send button and mean something different, so the wording and the
	// finalizing glyph follow the destination.
	const inserts = () => !!props.onTranscript;
	// Material Symbols Rounded ligature per state.
	const micIcon = () => {
		return voiceState() === "listening"
			? "stop"
			: voiceState() === "finalizing"
				? inserts()
					? "keyboard_return"
					: "send"
				: "mic";
	};

	return (
		<div
			class={props.variant === "inline" ? "voice-input voice-input--inline" : "voice-input"}
			data-testid="mobile-voice-input"
			data-state={voiceState()}
			data-engine={!engineAvailable ? "unavailable" : useDeepgram() ? "deepgram" : "web-speech"}
		>
			{/* Transcript caption (M3 snackbar). */}
			<Show when={showCaption()}>
				<div
					class={
						dispError() ? "voice-caption voice-caption--error" : "voice-caption"
					}
					data-testid="voice-caption"
				>
					<Show
						when={dispError()}
						fallback={
							<>
								<span class="voice-caption__final">{dispFinal()}</span>
								<Show when={dispInterim()}>
									<span class="voice-caption__interim">
										{dispFinal() ? " " : ""}
										{dispInterim()}
									</span>
								</Show>
								<Show when={!dispFinal() && !dispInterim()}>
									<span class="voice-caption__hint">
										{voiceState() === "finalizing" ? (inserts() ? "Inserting…" : "Sending…") : "Listening…"}
									</span>
								</Show>
							</>
						}
					>
						<span class="voice-caption__error">Mic error: {dispError()}</span>
					</Show>
				</div>
			</Show>

			{/* ✕ discard (small tonal FAB) + mic (stop = send), side by side. */}
			<div class="voice-input__cluster">
				<Show when={isActive()}>
					<button
						type="button"
						class="voice-fab voice-fab--discard"
						data-testid="voice-discard"
						onClick={() => discard()}
						aria-label="Discard recording"
					>
						<span class="voice-fab__icon">close</span>
					</button>
				</Show>

				<button
					type="button"
					class="voice-fab"
					data-testid="voice-mic"
					data-recording={voiceState() === "listening" ? "true" : "false"}
					data-finalizing={voiceState() === "finalizing" ? "true" : "false"}
					onPointerDown={(e) => {
						if (props.variant !== "inline") onFabPointerDown(e);
						// Open the device and fetch the key on press, not on release:
						// the cold getUserMedia is what used to eat the first words.
						if (voiceState() === "idle" && useDeepgram()) {
							void warmMic().catch(() => {
								/* startCapture surfaces the error on the actual tap */
							});
							prefetchDeepgramKey();
						}
					}}
					onClick={() => toggleRecord()}
					aria-label={
						voiceState() === "listening"
							? inserts()
								? "Stop and insert"
								: "Stop and send"
							: voiceState() === "finalizing"
								? inserts()
									? "Inserting"
									: "Sending"
								: "Start recording"
					}
				>
					<span class="voice-fab__icon">{micIcon()}</span>
				</button>
			</div>
		</div>
	);
};
