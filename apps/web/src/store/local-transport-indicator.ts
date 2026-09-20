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

export interface TerminalTransportPresentation {
  readonly kind: TerminalTransportKind | null;
  readonly label: string;
  readonly description: string;
}

const LOOPBACK_PRESENTATION = Object.freeze<TerminalTransportPresentation>({
  kind: "loopback",
  label: "Loopback",
  description: "Terminal cells and input use a direct connection on this device.",
});
const WEBRTC_PRESENTATION = Object.freeze<TerminalTransportPresentation>({
  kind: "webrtc",
  label: "WebRTC",
  description: "Terminal cells and input use a direct WebRTC connection to the worker.",
});
const SYNC_PRESENTATION = Object.freeze<TerminalTransportPresentation>({
  kind: "sync",
  label: "Coordinator",
  description: "Terminal cells and input go through the coordinator over Sync.",
});
const WAITING_PRESENTATION = Object.freeze<TerminalTransportPresentation>({
  kind: null,
  label: "Waiting",
  description: "No transport is confirmed for the current terminal screen.",
});

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

/** Stable visible carrier presentation for selected terminal headers. */
export function sessionTerminalTransportPresentation(
  sessionId: string,
): TerminalTransportPresentation {
  switch (sessionTerminalTransportKind(sessionId)) {
    case "loopback":
      return LOOPBACK_PRESENTATION;
    case "webrtc":
      return WEBRTC_PRESENTATION;
    case "sync":
      return SYNC_PRESENTATION;
    default:
      return WAITING_PRESENTATION;
  }
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
