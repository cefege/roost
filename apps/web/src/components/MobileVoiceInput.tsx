// MobileVoiceInput — the terminal composer's inline mic.
// State: idle → starting → listening → finalizing → insert. The tap
// synchronously enters "starting" (double-taps can never double-open the
// device); only an engine report of mic+transport live leaves it. Tap again
// to finalize into the draft; ✕ discards engine result and provisional tail.
//
// If a Deepgram key is configured in coord, dictation streams to Deepgram;
// otherwise it uses the browser's built-in Web Speech recognizer. The
// tap/finalize/insert UI is identical for both.

import type { Component } from "solid-js";
import {
	Show,
	createEffect,
	createSignal,
	onCleanup,
	onMount,
} from "solid-js";
import type { SessionId } from "@roost/shared/wire";
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
import { isTouchDevice } from "../lib/windowSizeClass.ts";
import {
	activeVoiceOwner,
	ensureTranscriptionConfig,
	setActiveVoiceOwner,
	transcriptionConfig,
} from "../lib/voiceState.ts";
import { createTrackedTimeouts } from "./trackedTimeout.ts";

// The capture pipeline stays warm after a recording so the next tap skips a
// 1–2 s cold device open. On a PHONE that window is also how long iOS/Android
// keep their recording indicator lit — a minute of orange dot after every
// dictation reads as "this app is still listening", and it hands iOS a whole
// minute in which to suspend the AudioContext under an idle-but-open pipeline.
// Long enough to chain a stop-and-retap, short enough that the indicator clears
// with the recording. Desktop keeps the long hold: no indicator, and the cold
// open is the only thing that ever ate the first spoken word.
if (isTouchDevice()) micIdle.releaseMs = 4_000;

// Every recording belongs to one concrete component instance. The immutable
// session ID provides globally meaningful diagnostics; the token prevents a
// responsive replacement for that same session from rendering a second live
// mic or releasing the first instance's claim.
let nextVoiceOwnerToken = 0;

// ─── voice recognition shim (built-in fallback) ──────────────────────────
interface SpeechRecognitionAlternative { transcript: string; confidence: number }
interface SpeechRecognitionResult {
	isFinal: boolean;
	length: number;
	item(index: number): SpeechRecognitionAlternative;
	[index: number]: SpeechRecognitionAlternative;
}
interface SpeechRecognitionResultList {
	length: number;
	item(index: number): SpeechRecognitionResult;
	[index: number]: SpeechRecognitionResult;
}
interface SpeechRecognitionEvent extends Event { resultIndex: number; results: SpeechRecognitionResultList }
interface SpeechRecognitionErrorEvent extends Event { error: string; message: string }
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

// starting = opening mic/transport (untappable-to-stop); listening = recording;
// finalizing = stopped, waiting for the engine to settle before the auto-send.

type VoiceState = "idle" | "starting" | "listening" | "finalizing";
interface Props {
	ownerId: SessionId;
	/** False when this composer's terminal surface is parked or covered. */
	active: boolean;
	onActiveChange?: (active: boolean) => void;
	/** Inserts finalized speech into the owning composer's retained draft. */
	onTranscript: (text: string) => void;
	/** Replaces the provisional tail with live speech. null ends the dictation
	 *  without a commit: the caller keeps base + finalized words and drops the
	 *  unfinalized tail (see onFinalTranscript), so an abandoned recording never
	 *  eats settled words nor bakes a hypothesis into the draft. */
	onLiveTranscript: (text: string | null) => void;
	/** Fires on every engine final update while dictating. The caller uses it
	 *  as the keep-on-abandon snapshot: finals are real text, the interim tail
	 *  is not. */
	onFinalTranscript?: (finalText: string) => void;
	// Explicit ✕: caller restores its own pre-mic baseline (distinct from null).
	onDiscard: () => void;
	readContext?: () => TerminalContext;
}

// ─── component ───────────────────────────────────────────────────────────

// The engine owns the normal exit from `finalizing` (onEnd / onFailure). This is
// the floor under it: no engine bug may leave the mic breathing and untappable
// forever. Comfortably longer than dictationTimings.finalizeWaitMs so it only
// ever fires when the engine did not.
const FINALIZE_WATCHDOG_MS = 6_000;

export const MobileVoiceInput: Component<Props> = (props) => {
	const [voiceState, setVoiceState] = createSignal<VoiceState>("idle");
	const [interimText, setInterimText] = createSignal("");
	const [finalText, setFinalText] = createSignal("");
	const [errorMsg, setErrorMsg] = createSignal<string | null>(null);
	const setTimeoutTracked = createTrackedTimeouts();
	const ownerToken = ++nextVoiceOwnerToken;
	const ownsVoice = () => activeVoiceOwner()?.token === ownerToken;
	const claimVoice = (): boolean => {
		const owner = activeVoiceOwner();
		if (owner && owner.token !== ownerToken) return false;
		if (!owner) setActiveVoiceOwner({ sessionId: props.ownerId, token: ownerToken });
		return true;
	};
	const releaseVoice = () => {
		if (ownsVoice()) setActiveVoiceOwner(null);
	};
	createEffect(() => props.onActiveChange?.(voiceState() !== "idle"));

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
		// Device AND transport live — the tap's promise is kept. Until this fires
		// the mic sits in "starting" where taps are deliberate no-ops.
		onLive: () => promoteToListening(),
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
		const finalPart = dispFinal();
		const live = `${finalPart} ${dispInterim()}`.trim();
		if (voiceState() === "idle") return;
		// Open the caller's session first: its reset must not erase the snapshot
		// this same engine update is about to deliver.
		props.onLiveTranscript(live);
		props.onFinalTranscript?.(finalPart);
	});

	let recognition: AnySpeechRecognition | null = null;
	// Web Speech `onend` fires for BOTH stop()-to-send and abort()-to-discard;
	// this flag tells them apart (Deepgram tracks its own endIntent internally).
	let webIntent: "send" | "cancel" | null = null;
	// Floor under `finalizing` — see FINALIZE_WATCHDOG_MS.
	let finalizeWatchdog: ReturnType<typeof setTimeout> | null = null;
	const clearFinalizeWatchdog = () => {
		if (finalizeWatchdog) {
			clearTimeout(finalizeWatchdog);
			finalizeWatchdog = null;
		}
	};

	onMount(() => {
		if (!webSupported) return;
		recognition = createSpeechRecognition();
		recognition.onstart = () => promoteToListening();
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
			props.onLiveTranscript(null);
			setVoiceState("idle");
			releaseVoice();
			setTimeoutTracked(() => setErrorMsg(null), 3000);
		};
	});

	onCleanup(() => {
		clearFinalizeWatchdog();
		if (voiceState() !== "idle") {
			forceFinish(false); // unmount mid-dictation keeps settled words only
		} else {
			releaseVoice();
			try {
				recognition?.abort();
			} catch {
				/* ignore */
			}
		}
		props.onActiveChange?.(false);
		recognition = null;
	});

	const startRecording = () => {
		clearFinalizeWatchdog();
		if (!props.active || !claimVoice()) return;
		const deepgram = useDeepgram();
		// Clear the selected engine before any async open. The live-feed effect
		// ignores these reset writes instead of reopening the prior baseline.
		if (deepgram) {
			dg.reset();
		} else {
			webIntent = null;
			setFinalText("");
			setInterimText("");
		}
		// Synchronously claim the button: from this instant a second tap hits the
		// "starting" no-op instead of re-entering the device open.
		setVoiceState("starting");
		if (deepgram) {
			dg.start();
		} else {
			try {
				recognition?.start(); // onstart promotes to listening
			} catch {
				/* already running */
			}
		}
	};
	// Mic attached AND transport open — the tap's promise is kept.
	const promoteToListening = () => {
		setVoiceState((s) => (s === "starting" ? "listening" : s));
	};

	// Stop recording and send automatically. Moves to `finalizing`; the actual
	// send fires from the engine's onEnd (Deepgram) / onend (Web Speech).
	const stopAndSend = () => {
		setVoiceState("finalizing");
		finalizeWatchdog = setTimeout(() => {
			finalizeWatchdog = null;
			if (voiceState() === "finalizing") forceFinish();
		}, FINALIZE_WATCHDOG_MS);
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
		else if (s === "starting") return; // open in flight — a second tap must not double-start
		else if (s === "listening") stopAndSend();
		else forceFinish(); // finalizing: a second tap hurries the send, never wedges
	};

	// Reset transcript/state and release the singleton recording slot.
	const resetToIdle = () => {
		clearFinalizeWatchdog();
		// Mark idle before clearing engine text so the live-feed effect cannot
		// reopen a baseline with reset-generated empty text.
		setVoiceState("idle");
		dg.reset();
		setFinalText("");
		setInterimText("");
		releaseVoice();
	};
	// A failed recording must LEAVE its state — the engine tore itself down and
	// error() holds the reason nothing here may reset. Releasing the voice slot
	// matters equally: while it is held, no other instance may record.
	const failToIdle = () => {
		clearFinalizeWatchdog();
		if (voiceState() === "idle") return;
		props.onLiveTranscript(null);
		setFinalText("");
		setInterimText("");
		setVoiceState("idle");
		releaseVoice();
	};

	// Called from the engine's end callback. Final speech belongs in the draft;
	// the composer's explicit send action is the only PTY submission path.
	const deliver = (text: string) => {
		if (text.length === 0) {
			props.onLiveTranscript(null);
			resetToIdle();
			return;
		}
		props.onTranscript(text);
		resetToIdle();
	};
	const sendTranscript = () => { deliver(`${dispFinal()} ${dispInterim()}`.trim()); };
	// Escape from finalizing/starting (hurried tap, deactivation, unmount):
	// deliver what settled and tear down — never cost the user their words.
	// keepInterim=true (hurried tap) commits the painted tail the user sees;
	// involuntary exits pass false so an engine hypothesis the user never
	// got to accept is not baked into the draft behind their back.
	const forceFinish = (keepInterim = true) => {
		clearFinalizeWatchdog();
		const text = keepInterim
			? `${dispFinal()} ${dispInterim()}`.trim()
			: dispFinal().trim();
		if (useDeepgram()) dg.abort();
		else {
			webIntent = "cancel";
			try {
				recognition?.abort();
			} catch {
				/* ignore */
			}
		}
		deliver(text);
	};

	// ✕ discards without sending. Routed through onDiscard: restoring the
	// baseline is the CALLER's move, unlike null-keeps-painted semantics.
	const discard = () => {
		clearFinalizeWatchdog();
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
		props.onDiscard();
		resetToIdle();
	};

	// A covered composer must never keep recording without reachable controls —
	// but switching away commits what settled instead of silently discarding.
	createEffect(() => {
		if (!props.active && voiceState() !== "idle") forceFinish(false);
	});

	const isActive = () => voiceState() !== "idle";
	// Material Symbols ligature per state; finalizing inserts into the draft
	// (intentionally no direct-send voice mode).
	const micIcon = () =>
		({ idle: "mic", starting: "progress_activity", listening: "stop", finalizing: "keyboard_return" })[voiceState()];

	// Every instance keeps its mic mounted and clickable — responsive swaps
	// create two instances per session, and unmounting the idle twin made the
	// button jump. Ownership arbitrates: only the owner shows recording state.
	return (
		<div
			class="voice-input voice-input--inline"
			data-testid="mobile-voice-input"
			data-state={voiceState()}
			data-owner-active={ownsVoice() ? "true" : "false"}
			data-engine={!engineAvailable ? "unavailable" : useDeepgram() ? "deepgram" : "web-speech"}
		>
			{/* Errors need their own surface; transcript text lives in the field. */}
			<Show when={dispError()}>
				{(error) => (
					<div
						class="voice-caption voice-caption--error"
						data-testid="voice-caption"
					>
						<span class="voice-caption__error">Mic error: {error()}</span>
					</div>
				)}
			</Show>

			{/* Active extras (✕ discard) belong to the owner alone. */}
			<div class="voice-input__cluster">
				<Show when={isActive() && ownsVoice()}>
					<button
						type="button"
						class="voice-fab voice-fab--discard"
						data-testid="voice-discard"
						onMouseDown={(event) => event.preventDefault()}
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
					data-starting={voiceState() === "starting" ? "true" : "false"}
					data-finalizing={voiceState() === "finalizing" ? "true" : "false"}
					aria-busy={voiceState() === "starting" ? "true" : undefined}
					onMouseDown={(event) => event.preventDefault()}
					onPointerDown={() => {
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
						({ idle: "Start recording", starting: "Starting recording", listening: "Stop and insert", finalizing: "Inserting" })[voiceState()]
					}
				>
					<span class="voice-fab__icon">{micIcon()}</span>
				</button>
			</div>
		</div>
	);
};
