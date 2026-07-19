// MobileVoiceInput — bottom-right mic button + live transcript.
// State: idle → listening → finalizing → (auto-send). Tap to start; tap again
// to stop, which finalizes and sends automatically — no review step. While
// recording (or finalizing) an ✕ button discards without sending. Send writes
// the transcript as PTY input (bracketed paste + CR), same path as keyboard.
//
// Engine: if a Deepgram key is configured in coord, dictation streams to
// Deepgram (deepgramDictation); otherwise it uses the browser's built-in Web
// Speech recognizer. The tap/finalize/auto-send UI is identical for both.
// Callers: CellTerminal.tsx.

import type { Component } from "solid-js";
import {
	Show,
	createResource,
	createSignal,
	onCleanup,
	onMount,
} from "solid-js";
import type { ChannelId } from "@roost/shared/wire";
import { coordClient } from "../connect.ts";
import { createDeepgramDictation } from "../lib/deepgramDictation.ts";
import { buildPtyPayload } from "../lib/ptyPaste.ts";
import {
	buildAccum,
	finalizeKeyterms,
	type TerminalContext,
} from "../lib/keytermContext.ts";
import { learnTerms, lexiconTopTerms } from "../lib/keytermLexicon.ts";
import { keytermBiasing } from "../lib/keytermBiasingPref.ts";
import { onFabPointerDown } from "../lib/fabDragOffset.ts";
import { dragArmed } from "../lib/dragThreshold.ts";
// ─── shared recording state ───────────────────────────────────────────────
// All MobileVoiceInput instances share this signal. Only one can record at a
// time — the owning instance renders; all others return null early. This
// prevents multiple position:fixed elements from overlapping at the same
// viewport coordinates when multiple terminals are visible.
export const [activeVoiceChannel, setActiveVoiceChannel] = createSignal<number | null>(null);

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

// ─── bracketed paste ────────────────────────────────────────────────────
// buildPtyPayload is shared with TerminalContextMenu (see lib/ptyPaste.ts).
// Dictation appends its own CR (separate frame); paste does not.

const CR_BYTES = new TextEncoder().encode("\r");

// The receiver needs time to ingest a big message before the Enter lands, or it
// gets eaten. Scale with length: 150ms floor + ~0.4ms/char, capped at 2.5s.
function enterDelayMs(text: string): number {
	return Math.min(150 + Math.ceil(text.length * 0.4), 2500);
}

// Sustained press (no drag) on the mic FAB opens the type-mode composer
// instead of recording. A quick tap still records.
const LONG_PRESS_MS = 500;

// ─── types ───────────────────────────────────────────────────────────────

// listening = mic recording; finalizing = stopped, waiting for the engine to
// settle the last words before the auto-send fires.
type VoiceState = "idle" | "listening" | "finalizing";

interface Props {
	channelId: ChannelId;
	sendInput: (channelId: ChannelId, data: Uint8Array) => void;
	// Snapshot of the terminal you're dictating into — read at recording start to
	// bias Deepgram toward on-screen jargon (keytermContext). Deepgram-only.
	readContext?: () => TerminalContext;
	// Re-grab the hidden wterm textarea after a send/discard. The mic <button>
	// holds DOM focus while recording; auto-send fires from an engine callback
	// (no click/focus event) so nothing re-focuses the terminal → the next Enter
	// hits a global handler (jumps workspace) instead of the PTY. CellTerminal
	// passes term.forceFocus() here.
	refocusTerminal?: () => void;
}

// ─── component ───────────────────────────────────────────────────────────

export const MobileVoiceInput: Component<Props> = (props) => {
	const [voiceState, setVoiceState] = createSignal<VoiceState>("idle");
	const [interimText, setInterimText] = createSignal("");
	const [finalText, setFinalText] = createSignal("");
	const [errorMsg, setErrorMsg] = createSignal<string | null>(null);
	// Type mode: long-press the mic → keyboard composer instead of recording.
	// Kept separate from VoiceState (engine callbacks branch on that union);
	// recording and typing are mutually exclusive, guarded below.
	const [typing, setTyping] = createSignal(false);
	const [draft, setDraft] = createSignal("");
	let composeEl: HTMLTextAreaElement | undefined;
	// If another session owns the recording, hide completely. Only ONE MobileVoiceInput
	// DOM tree exists at a time — no position:fixed overlap when multiple terminals are visible.
	const owner = activeVoiceChannel();
	if (owner !== null && owner !== props.channelId) return null;

	// Deepgram config from coord (which engine + language).
	const [config] = createResource(() => coordClient.transcriptionGetConfig({}));
	const dg = createDeepgramDictation({
		language: () => config()?.deepgramLanguage ?? "en",
		grantToken: () =>
			coordClient.transcriptionGrantToken({}).then((r) => ({
				accessToken: r.accessToken,
				expiresIn: r.expiresIn,
			})),
		// Engine finished finalizing after a stop() → send whatever settled.
		onEnd: () => sendTranscript(),
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

	const useDeepgram = () => !!config()?.deepgramConfigured && dg.supported;
	const webSupported = isWebSpeechSupported();
	if (!webSupported && !dg.supported) return null;

	// Unified getters so the JSX is engine-blind.
	const dispFinal = () => (useDeepgram() ? dg.final() : finalText());
	const dispInterim = () => (useDeepgram() ? dg.interim() : interimText());
	const dispError = () => (useDeepgram() ? dg.error() : errorMsg());

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
			setVoiceState("idle");
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

	// Called from the engine's end callback (auto-send) — NOT a button anymore.
	const sendTranscript = () => {
		const text = `${dispFinal()} ${dispInterim()}`.trim();
		if (text.length === 0) {
			resetToIdle();
			return;
		}
		props.sendInput(props.channelId, buildPtyPayload(text));
		// Enter as its own frame, after a length-scaled delay, so it reliably
		// submits even very long messages.
		setTimeout(
			() => props.sendInput(props.channelId, CR_BYTES),
			enterDelayMs(text),
		);
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
		resetToIdle();
	};

	// ─── type mode ───────────────────────────────────────────────────────────
	// Claim the shared slot exactly like startRecording so sibling FABs (other
	// terminals' mics, agent-launch/plan) stay hidden and don't cover the box.
	const startTyping = () => {
		setDraft("");
		setTyping(true);
		setActiveVoiceChannel(props.channelId);
	};

	// Close the composer and hand focus back to the terminal, using the same
	// guarded refocus as resetToIdle (only if focus is still inside our UI).
	const closeTyping = () => {
		setTyping(false);
		setDraft("");
		setActiveVoiceChannel(null);
		const voiceEl = document.querySelector('[data-testid="mobile-voice-input"]');
		if (!voiceEl || voiceEl.contains(document.activeElement)) {
			props.refocusTerminal?.();
		}
	};

	// Send typed text through the same PTY pipeline dictation uses: bracketed
	// paste, then CR as its own frame after a length-scaled delay.
	const sendDraft = () => {
		const text = draft().trim();
		if (text.length === 0) {
			closeTyping();
			return;
		}
		props.sendInput(props.channelId, buildPtyPayload(text));
		setTimeout(
			() => props.sendInput(props.channelId, CR_BYTES),
			enterDelayMs(text),
		);
		closeTyping();
	};

	// One-shot capture-phase click swallow — mirrors fabDragOffset.onUp so the
	// trailing click after a long-press doesn't fire toggleRecord.
	const swallowNextClick = () => {
		const onClickCapture = (ev: MouseEvent) => {
			ev.stopPropagation();
			ev.preventDefault();
			window.removeEventListener("click", onClickCapture, true);
		};
		window.addEventListener("click", onClickCapture, true);
	};

	// Long-press (no drag) on the mic FAB opens type mode. Quick tap records
	// (via onClick); drag ≥8px repositions the cluster (via onFabPointerDown).
	const onMicPointerDown = (e: PointerEvent) => {
		onFabPointerDown(e); // preserve FAB drag-to-reposition
		if (e.pointerType === "mouse" && e.button !== 0) return;
		if (voiceState() !== "idle" || typing()) return;
		const start = { x: e.clientX, y: e.clientY };
		let fired = false;
		const timer = window.setTimeout(() => {
			fired = true;
			cleanup();
			startTyping();
			swallowNextClick();
		}, LONG_PRESS_MS);
		const onMove = (ev: PointerEvent) => {
			if (dragArmed(start, ev.clientX, ev.clientY)) cleanup();
		};
		const cleanup = () => {
			clearTimeout(timer);
			window.removeEventListener("pointermove", onMove);
			window.removeEventListener("pointerup", onEnd);
			window.removeEventListener("pointercancel", onEnd);
		};
		const onEnd = () => {
			if (!fired) cleanup();
		};
		window.addEventListener("pointermove", onMove);
		window.addEventListener("pointerup", onEnd);
		window.addEventListener("pointercancel", onEnd);
	};

	const isActive = () => voiceState() !== "idle";
	// Material Symbols Rounded ligature per state.
	const micIcon = () => {
		if (typing()) return "send";
		return voiceState() === "listening"
			? "stop"
			: voiceState() === "finalizing"
				? "send"
				: "mic";
	};

	return (
		<div
			class="voice-input"
			data-testid="mobile-voice-input"
			data-state={voiceState()}
			data-engine={useDeepgram() ? "deepgram" : "web-speech"}
		>
			{/* Transcript caption (M3 snackbar). */}
			<Show when={isActive() || dispError()}>
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
										{voiceState() === "finalizing" ? "Sending…" : "Listening…"}
									</span>
								</Show>
							</>
						}
					>
						<span class="voice-caption__error">Mic error: {dispError()}</span>
					</Show>
				</div>
			</Show>

			{/* Type-mode composer (keyboard input) — long-press the mic to open. */}
			<Show when={typing()}>
				<div class="voice-compose" data-testid="voice-compose">
					<textarea
						class="voice-compose__input"
						data-testid="voice-compose-input"
						rows={2}
						placeholder="Type a message…"
						value={draft()}
						onInput={(e) => setDraft(e.currentTarget.value)}
						onKeyDown={(e) => {
							if (e.key === "Enter" && !e.shiftKey) {
								e.preventDefault();
								sendDraft();
							} else if (e.key === "Escape") {
								e.preventDefault();
								closeTyping();
							}
						}}
						ref={(el) => {
							composeEl = el;
							queueMicrotask(() => el.focus());
						}}
					/>
				</div>
			</Show>

			{/* ✕ discard (small tonal FAB) + mic (stop = send), side by side. */}
			<div class="voice-input__cluster">
				<Show when={isActive() || typing()}>
					<button
						type="button"
						class="voice-fab voice-fab--discard"
						data-testid="voice-discard"
						onClick={() => (typing() ? closeTyping() : discard())}
						aria-label={typing() ? "Discard message" : "Discard recording"}
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
					data-typing={typing() ? "true" : "false"}
					onPointerDown={onMicPointerDown}
					onClick={() => (typing() ? sendDraft() : toggleRecord())}
					aria-label={
						typing()
							? "Send message"
							: voiceState() === "listening"
								? "Stop and send"
								: voiceState() === "finalizing"
									? "Sending"
									: "Start recording"
					}
				>
					<span class="voice-fab__icon">{micIcon()}</span>
				</button>
			</div>
		</div>
	);
};
