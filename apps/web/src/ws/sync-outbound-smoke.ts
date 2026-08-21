// Smoke-only observation for complete terminal input batches.
// The hook is unavailable in ordinary documents and receives an owned copy.

export type SmokeTerminalInputObserver = (sessionId: string, bytes: Uint8Array) => void;

let smokeTerminalInputObserver: SmokeTerminalInputObserver | null = null;

export function setSmokeTerminalInputObserver(
  observer: SmokeTerminalInputObserver | null,
): void {
  try {
    if (typeof localStorage === "undefined" || localStorage.getItem("roostSmoke") !== "1") return;
  } catch {
    return;
  }
  smokeTerminalInputObserver = observer;
}

export function currentSmokeTerminalInputObserver(): SmokeTerminalInputObserver | null {
  return smokeTerminalInputObserver;
}

export function _resetSmokeOutboundForTest(): void {
  smokeTerminalInputObserver = null;
}
