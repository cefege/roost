// Defines TerminalScreenHub's public ingress, snapshot, and socket contracts.
// The hub owns canonical cache behavior while callers provide these boundary callbacks.
// Keeping the vocabulary separate lets consumers depend on the contract without cache internals.

import type { FirehoseFrame } from "@roost/shared/proto/sync_pb";
import type { TerminalSnapshotSource } from "./terminal-screen-frames.ts";

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
  setTimer?: SetTerminalScreenHubTimer;
  clearTimer?: ClearTerminalScreenHubTimer;
  now?: () => number;
}
