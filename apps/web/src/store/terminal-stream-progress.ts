// Baseline progress is pushed from assembler state transitions, never polled.
// A session replica fans one progress transition only to its own view handles.
// View handles retain the existing subscribeProgress consumer contract.

import type {
  BaselineProgress,
  TerminalSessionReplica,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";

export function subscribeTerminalBaselineProgress(
  view: TerminalViewRecord,
  listener: (progress: BaselineProgress | null) => void,
): () => void {
  if (view.disposed) return () => undefined;
  view.progressListeners.add(listener);
  const progress = view.session.assembler.snapshotProgress;
  view.lastProgressKey = baselineProgressKey(progress);
  listener(progress);
  return () => {
    view.progressListeners.delete(listener);
    if (view.progressListeners.size === 0) view.lastProgressKey = null;
  };
}

export function notifyTerminalBaselineProgress(session: TerminalSessionReplica): void {
  const progress = session.assembler.snapshotProgress;
  const key = baselineProgressKey(progress);
  for (const view of session.handles.values()) {
    if (view.disposed || view.progressListeners.size === 0 || view.lastProgressKey === key) {
      continue;
    }
    view.lastProgressKey = key;
    for (const listener of view.progressListeners) listener(progress);
  }
}

function baselineProgressKey(progress: BaselineProgress | null): string {
  return progress === null
    ? "idle"
    : `${progress.snapshotId}:${progress.receivedChunks}/${progress.totalChunks}`;
}
