// Reactive transport state for terminal tabs. A route is visible only after
// its elected canonical replica has a baseline, so candidates never leak into
// UI state. Registry and canonical-baseline transitions advance one revision.

import { terminalDirectRegistry } from "./terminal-stream-transport.ts";
import {
  notifyTerminalTransportStateChange,
  terminalSessions,
  terminalTransportRevision,
} from "./terminal-stream-state.ts";
import {
  terminalGenerationTokenEquals,
  type TerminalTransportKind,
} from "./terminal-stream-types.ts";

let installed = false;
let refreshQueued = false;

/** Installs one document-level refresh bridge before terminal tabs render. */
export function installTerminalTransportIndicator(): void {
  if (installed) return;
  installed = true;
  terminalDirectRegistry.subscribe(() => {
    if (refreshQueued) return;
    refreshQueued = true;
    queueMicrotask(() => {
      refreshQueued = false;
      notifyTerminalTransportStateChange();
    });
  });
}

/** The elected carrier only after it owns a complete canonical baseline. */
export function sessionTerminalTransportKind(
  sessionId: string,
): TerminalTransportKind | null {
  terminalTransportRevision();
  const session = terminalSessions.get(sessionId);
  if (!session?.baselineReady || !session.generation) return null;
  if (session.generation.transportKind === "sync") return "sync";
  const direct = terminalDirectRegistry.activeForSession(sessionId);
  return direct && terminalGenerationTokenEquals(direct.token(), session.generation)
    ? session.generation.transportKind
    : null;
}

/** Human-facing carrier label for native tooltip and hover-card surfaces. */
export function sessionTerminalTransportLabel(sessionId: string): string | null {
  switch (sessionTerminalTransportKind(sessionId)) {
    case "loopback":
      return "Direct on this device";
    case "webrtc":
      return "Direct peer connection";
    default:
      return null;
  }
}

/** True only while an elected direct route still has a current terminal proof. */
export function hasLivenessQualifiedDirectTerminal(): boolean {
  terminalTransportRevision();
  for (const [sessionId, session] of terminalSessions) {
    const token = session.generation;
    if (
      !session.baselineReady
      || !token
      || token.transportKind === "sync"
      || session.lastAcceptedFrameAtMs === null
      || !terminalGenerationTokenEquals(session.lastAcceptedFrameGeneration, token)
    ) continue;
    const direct = terminalDirectRegistry.activeForSession(sessionId);
    if (!direct || !terminalGenerationTokenEquals(direct.token(), token)) continue;
    if (token.transportKind === "webrtc" && direct.telemetry?.().livenessQualified !== true) continue;
    return true;
  }
  return false;
}
