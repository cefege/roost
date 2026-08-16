// Predictive local-echo display preference. Persistence is read once at module
// initialization and mirrored by a typed Solid signal; the keystroke/frame hot
// path receives this accessor and never touches localStorage.

import { createSignal } from "solid-js";

const STORAGE_KEY = "roostPredict";

export type PredictMode = "adaptive" | "always" | "never" | "experimental";

/** Normalize values shipped by older builds while failing unknown settings
 * closed to the safe adaptive mode. */
export function normalizePredictMode(value: string | null): PredictMode {
  if (value === "0" || value === "never") return "never";
  if (value === "force" || value === "always") return "always";
  if (value === "experimental") return "experimental";
  return "adaptive";
}

let initialMode: PredictMode = "adaptive";
try {
  initialMode = normalizePredictMode(localStorage.getItem(STORAGE_KEY));
} catch {
  // Storage can be disabled; adaptive remains the deterministic default.
}

const [predictMode, setModeSignal] = createSignal<PredictMode>(initialMode);

/** Reactive accessor injected into each pane's PredictiveEcho instance. */
export { predictMode };

export function setPredictMode(value: string): void {
  const mode = normalizePredictMode(value);
  setModeSignal(mode);
  try {
    localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    // Preference remains live for this document even when persistence is denied.
  }
}
