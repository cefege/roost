// predictPref — predictive local echo display mode
// (display_preference: Adaptive / Always / Never
// / Experimental). Persisted per device to localStorage.roostPredict — the SAME
// key lib/predictiveEcho.ts reads each call (engine stays framework-free and
// test-stubbable). This module is the reactive Settings binding: a Solid signal
// mirrored to localStorage so the Select and the engine never drift (localStorage
// is authoritative; the engine re-reads it per call).
//
// Default Adaptive: engages only on a high-latency link. On a fast
// LAN/tailnet it is a no-op by design. Always shows predictions everywhere except
// fullscreen (alt-screen) apps. Experimental shows guesses with zero latency but
// may flicker. Never is the kill switch. Toggled from Settings → Terminal.

import { createSignal } from "solid-js";

const STORAGE_KEY = "roostPredict";

function load(): string {
  try {
    return localStorage.getItem(STORAGE_KEY) ?? "adaptive";
  } catch {
    return "adaptive";
  }
}

const [predictMode, _set] = createSignal<string>(load());

/** Reactive accessor — read inside JSX so the Select reflects the live value. */
export { predictMode };

export type PredictMode = "adaptive" | "always" | "never" | "experimental";

export function setPredictMode(v: string): void {
  try { localStorage.setItem(STORAGE_KEY, v); } catch { /* ignore */ }
  _set(v);
}
