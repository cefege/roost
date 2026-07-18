// keytermBiasingPref — client-only UI pref: bias Deepgram dictation toward the
// terminal's on-screen jargon (keytermContext). Default ON — it's the feature;
// the toggle exists so you can A/B against plain transcription. Persisted to
// localStorage (per device) and reactive. Read by MobileVoiceInput's keyterms
// callback; toggled from Settings → Voice (TranscriptionPane).

import { createSignal } from "solid-js";

const STORAGE_KEY = "roost.keytermBiasing";

function load(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) !== "0"; // default ON
  } catch {
    return true;
  }
}

const [keytermBiasing, setKeytermBiasingSignal] = createSignal<boolean>(load());

/** Reactive accessor — read in the keyterms callback to gate biasing live. */
export { keytermBiasing };

/** Persist + flip the pref. Applies to the next recording. */
export function setKeytermBiasing(on: boolean): void {
  try {
    localStorage.setItem(STORAGE_KEY, on ? "1" : "0");
  } catch {
    // private-mode / storage-disabled: keep the in-memory signal only.
  }
  setKeytermBiasingSignal(on);
}
