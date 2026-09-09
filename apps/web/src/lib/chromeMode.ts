// Chrome mode owns this document's browser-local presentation preference.
// main.tsx applies the stored value before render; Settings changes it through setChromeMode.
// It depends only on Solid reactivity and browser storage/document APIs.
// It never owns theme tokens, terminal state, routing, or layout.

import { createSignal } from "solid-js";

const CHROME_MODE_STORAGE_KEY = "roost.chromeMode.v1";
const DEFAULT_CHROME_MODE: ChromeMode = "roost";

export type ChromeMode = "roost" | "workbench";

const [chromeModeSignal, setChromeModeSignal] = createSignal<ChromeMode>(DEFAULT_CHROME_MODE);

/** Reactive browser-local chrome mode for settings presentation. */
export const currentChromeMode = chromeModeSignal;

/** Read a valid persisted preference without mutating browser storage. */
export function loadChromeMode(): ChromeMode {
  try {
    if (typeof localStorage === "undefined") return DEFAULT_CHROME_MODE;
    return parseChromeMode(localStorage.getItem(CHROME_MODE_STORAGE_KEY));
  } catch {
    return DEFAULT_CHROME_MODE;
  }
}

/** Apply presentation state to this document without persisting it. */
export function applyChromeMode(mode: ChromeMode): void {
  const resolvedMode = parseChromeMode(mode);
  const root = typeof document === "undefined" ? undefined : document.documentElement;
  root?.setAttribute("data-chrome-mode", resolvedMode);
  setChromeModeSignal(resolvedMode);
}

/** Persist one explicit valid choice, then apply it to this document. */
export function setChromeMode(mode: ChromeMode): void {
  if (!isChromeMode(mode)) {
    applyChromeMode(DEFAULT_CHROME_MODE);
    return;
  }

  try {
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(CHROME_MODE_STORAGE_KEY, mode);
    }
  } catch {
    // Storage is optional; the explicit choice still applies in this document.
  }
  applyChromeMode(mode);
}

function isChromeMode(value: unknown): value is ChromeMode {
  return value === "roost" || value === "workbench";
}

function parseChromeMode(value: unknown): ChromeMode {
  return isChromeMode(value) ? value : DEFAULT_CHROME_MODE;
}

if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
  window.addEventListener("storage", (event: StorageEvent) => {
    if (event.key === CHROME_MODE_STORAGE_KEY || event.key === null) {
      applyChromeMode(parseChromeMode(event.newValue));
    }
  });
}
