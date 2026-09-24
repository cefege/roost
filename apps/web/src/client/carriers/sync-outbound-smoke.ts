// Smoke-only observation captures admitted input batches and settled outcome categories.
// Both observers install behind the VITE smoke gate; outcome callbacks carry status only.

export type SmokeTerminalInputObserver = (sessionId: string, bytes: Uint8Array) => void;
export type SmokeTerminalInputOutcome = "accepted" | "rejected" | "ambiguous";
export type SmokeTerminalInputOutcomeObserver = (
  sessionId: string,
  outcome: SmokeTerminalInputOutcome,
) => void;

let smokeTerminalInputObserver: SmokeTerminalInputObserver | null = null;
let smokeTerminalInputOutcomeObserver: SmokeTerminalInputOutcomeObserver | null = null;

export function setSmokeTerminalInputObserver(
  observer: SmokeTerminalInputObserver | null,
): void {
  // Build-time gate first: prod bundles fold the observer hook away entirely.
  if (import.meta.env.VITE_ROOST_SMOKE !== "1") return;
  try {
    if (typeof localStorage === "undefined" || localStorage.getItem("roostSmoke") !== "1") return;
  } catch {
    return;
  }
  smokeTerminalInputObserver = observer;
}
export function setSmokeTerminalInputOutcomeObserver(
  observer: SmokeTerminalInputOutcomeObserver | null,
): void {
  if (import.meta.env.VITE_ROOST_SMOKE !== "1") return;
  try {
    if (typeof localStorage === "undefined" || localStorage.getItem("roostSmoke") !== "1") return;
  } catch {
    return;
  }
  smokeTerminalInputOutcomeObserver = observer;
}

export function currentSmokeTerminalInputOutcomeObserver(): SmokeTerminalInputOutcomeObserver | null {
  return smokeTerminalInputOutcomeObserver;
}

export function currentSmokeTerminalInputObserver(): SmokeTerminalInputObserver | null {
  return smokeTerminalInputObserver;
}

export function _resetSmokeOutboundForTest(): void {
  smokeTerminalInputObserver = null;
  smokeTerminalInputOutcomeObserver = null;
}
