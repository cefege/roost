// Defines TerminalScreenHub's public ingress, snapshot, and socket contracts.
// The hub owns canonical cache behavior while callers provide these boundary callbacks.
// Keeping the vocabulary separate lets consumers depend on the contract without cache internals.

import type { FirehoseFrame } from "@roost/protocol/proto/sync_pb";
import type { TerminalSnapshotSource } from "./terminal-screen-frames.ts";
// Type-only on purpose: terminal-screen-budget.ts imports this hub's two hard
// maxima at runtime, so erasing this direction is what keeps the module graph
// acyclic once compiled.
import type { TerminalScreenCaps } from "./terminal-screen-budget.ts";

export type TerminalDeltaEnqueueResult = "queued" | "needs_snapshot" | "handled";

export interface TerminalScreenSocketSink {
  beginTerminalStream(sessionId: string, streamId: string): boolean;
  enqueueTerminalState(frame: FirehoseFrame, sessionId: string): void;
  replaceTerminalSnapshot(
    sessionId: string,
    streamId: string,
    source: TerminalSnapshotSource,
  ): boolean;
  enqueueTerminalDelta(
    sessionId: string,
    streamId: string,
    frame: FirehoseFrame,
  ): TerminalDeltaEnqueueResult;
  dropTerminalSession(sessionId: string): void;
}

export type TerminalScreenHubTimer = NodeJS.Timeout;

export type SetTerminalScreenHubTimer = (
  callback: () => void,
  delayMs: number,
) => TerminalScreenHubTimer;

export type ClearTerminalScreenHubTimer = (timer: TerminalScreenHubTimer) => void;

export interface TerminalScreenHubOptions {
  requestSnapshot(sessionId: string, streamId: string): void;
  unavailable?(sessionId: string, reason: string): void;
  requestFreshStream(sessionId: string, expectedStreamId: string, reason: string): void;
  fullAccepted?(sessionId: string, streamId: string): void;
  setTimer?: SetTerminalScreenHubTimer;
  clearTimer?: ClearTerminalScreenHubTimer;
  now?: () => number;
  /** Budget-derived residency ceilings; unset falls back to the hard maxima. */
  terminalScreen?: TerminalScreenCaps;
}
