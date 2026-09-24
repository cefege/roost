// Voice capture needs one shared owner so only the active session keeps the microphone.
// Terminal composers call this module when recording starts, stops, or changes sessions.
// Coordinator configuration is cached here to keep capture policy reactive across the UI.
// It also holds the one registration slot for the mounted composer's mic
// closures, so input devices with no pointer (a game controller) drive
// dictation through the same code a tap runs. Depends on: audioPcmCapture
// (warm-pipeline evidence), connect (coordinator config).

import { createSignal } from "solid-js";
import type { SessionId } from "@roost/protocol/wire";
import { isMicWarm } from "./audioPcmCapture.ts";
import { coordClient } from "../connect.ts";

export type VoiceOwner = { sessionId: SessionId; token: number };
const [activeVoiceOwner, setActiveVoiceOwner] = createSignal<VoiceOwner | null>(null);
export { activeVoiceOwner, setActiveVoiceOwner };

/** The mounted composer's mic closures. `toggle` means what a hardware button
 *  implies: not recording → open the mic; recording → stop AND SEND (never a
 *  silent discard); mid-finalize → hurry that same send. `discard` is the ✕. */
export interface VoiceControls {
  toggle(): void;
  discard(): void;
  /** Can a non-gesture caller (a controller) start dictation right now? A pad
   *  button is not a DOM event and carries no user activation, and outside that
   *  window getUserMedia is denied without even prompting (the incident
   *  deepgramDictation.ts records at its startCapture call). False until the
   *  permission is already granted or the pipeline is still warm from a tap. */
  canStartWithoutGesture(): boolean;
}

// One registration slot, so a pointer-free input device (a game controller)
// drives dictation by calling the composer's own closures instead of
// synthesising a click on a testid. Composers mount per pane and a focus switch
// mounts the next one BEFORE the old one's cleanup runs, so a deregistering
// instance MUST compare identity first: `if (voiceControls() === mine) …`.
const [voiceControls, setVoiceControls] = createSignal<VoiceControls | null>(null);
export { voiceControls };

export function registerVoiceControls(controls: VoiceControls | null): void {
  setVoiceControls(controls);
}

/** Wrap one composer's closures into the registered controls. The no-gesture
 *  policy lives here rather than in the caller: a start the browser would deny
 *  silently must leave the mic IDLE instead of painting a listening state
 *  nothing will ever end, so the refusal path is the composer's own failToIdle
 *  — already a no-op when it is idle, which is the state it must be left in. */
export function voiceControlsFor(closures: {
  toggleRecord(): void;
  failToIdle(): void;
  discard(): void;
}): VoiceControls {
  return {
    toggle: () => {
      // An open dictation always stops-and-sends; only STARTING needs a grant.
      if (voiceDictating() || micStartableWithoutGesture()) closures.toggleRecord();
      else closures.failToIdle();
    },
    discard: closures.discard,
    canStartWithoutGesture: micStartableWithoutGesture,
  };
}

/** True exactly while a dictation is open — the starting | listening |
 *  finalizing phases. Derived from the owner slot rather than a second signal:
 *  startRecording() claims it before entering "starting", and
 *  resetToIdle()/failToIdle()/unmount release it on the way back to idle. */
export function voiceDictating(): boolean {
  return activeVoiceOwner() !== null;
}

// The only honest evidence that a start carrying no user activation can open
// the mic: a permission this profile already granted, or a pipeline left warm
// by an earlier trusted tap.
const [micPermissionGranted, setMicPermissionGranted] = createSignal(false);
let micPermissionProbe: Promise<void> | null = null;

/** Latched by the composer when a recording reaches the live device: that open
 *  succeeded, so the next controller-driven start is not a dead end. */
export function noteMicPermissionGranted(): void {
  if (!micPermissionGranted()) setMicPermissionGranted(true);
}

/** Reading this schedules the one permission probe; the permission half is
 *  reactive so a grant landing later flips the controller's hint, while the
 *  warm-pipeline half is read fresh on every call. */
export function micStartableWithoutGesture(): boolean {
  probeMicPermission();
  return micPermissionGranted() || isMicWarm();
}

// Best-effort by construction: several engines reject the "microphone"
// descriptor outright, and a query that rejects must read as "needs a tap"
// rather than throw into a caller that is mid-button-press. A resolved
// non-granted state never clears the latch above — a real open outranks it.
function probeMicPermission(): void {
  if (micPermissionProbe) return;
  const permissions = typeof navigator === "undefined" ? undefined : navigator.permissions;
  if (!permissions?.query) {
    micPermissionProbe = Promise.resolve();
    return;
  }
  micPermissionProbe = permissions
    .query({ name: "microphone" as PermissionName })
    .then((status) => {
      if (status.state === "granted") setMicPermissionGranted(true);
      // A grant or revocation later in the session must reach the same hint.
      status.onchange = () => setMicPermissionGranted(status.state === "granted");
    })
    .catch(() => {});
}

export type TranscriptionConfig = { deepgramConfigured: boolean; deepgramLanguage: string };
const [transcriptionConfig, setTranscriptionConfig] = createSignal<TranscriptionConfig | null>(null);
export { transcriptionConfig };

let configFetch: Promise<unknown> | null = null;

/** Cache coordinator voice configuration across composer remounts. */
export function ensureTranscriptionConfig(): void {
  if (configFetch) return;
  let request: Promise<void>;
  request = coordClient
    .transcriptionGetConfig({})
    .then((config) => {
      setTranscriptionConfig({
        deepgramConfigured: config.deepgramConfigured,
        deepgramLanguage: config.deepgramLanguage,
      });
    })
    .catch(() => {
      if (configFetch === request) configFetch = null;
    });
  configFetch = request;
}
