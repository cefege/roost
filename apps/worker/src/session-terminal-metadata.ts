// Worker-owned semantic terminal metadata state and fair flush loop.
// Session emission records only bounded title/activity facts per channel; the
// transport capability selects this lane or old-coordinator raw compatibility.
// Reconnect replay reads retained facts after the durable snapshot barrier.

import {
  TERMINAL_METADATA_ACTIVITY_THROTTLE_MS,
  TerminalTitleParser,
} from "@roost/shared/terminal-metadata";
import { log } from "@roost/shared/log";
import type { SessionManager } from "./session-manager.ts";
import { disposeRawMetadataState } from "./session-raw-metadata.ts";
import type { TransportSendResult } from "./transport/coord-link-types.ts";

export const TERMINAL_METADATA_DISPATCH_FRAME_BUDGET = 32;

export interface TerminalMetadataState {
  parser: TerminalTitleParser;
  title: string | null;
  titleKey: string | null;
  activityTsMs: number | null;
  lastActivityPublishedAtMs: number | null;
  titleDirty: boolean;
  activityDirty: boolean;
}

/** Record one PTY chunk without retaining its raw bytes. */
export function observeTerminalMetadata(
  manager: SessionManager,
  channelId: number,
  bytes: Uint8Array,
): void {
  if (bytes.byteLength === 0) return;
  let state = manager.terminalMetadataByChannel.get(channelId);
  if (!state) {
    state = {
      parser: new TerminalTitleParser(),
      title: null,
      titleKey: null,
      activityTsMs: null,
      lastActivityPublishedAtMs: null,
      titleDirty: false,
      activityDirty: false,
    };
    manager.terminalMetadataByChannel.set(channelId, state);
  }
  const title = state.parser.push(bytes);
  let titleChanged = false;
  if (title && title.dedupKey !== state.titleKey) {
    state.title = title.title;
    state.titleKey = title.dedupKey;
    state.titleDirty = true;
    titleChanged = true;
  }
  const observedAtMs = Date.now();
  state.activityTsMs = observedAtMs;
  const activityDue =
    state.lastActivityPublishedAtMs === null
    || observedAtMs - state.lastActivityPublishedAtMs >= TERMINAL_METADATA_ACTIVITY_THROTTLE_MS;
  if (activityDue) state.activityDirty = true;
  if (
    manager.terminalMetadataNegotiated
    && (titleChanged || activityDue)
    && !manager.terminalMetadataReadyRing.has(channelId)
  ) {
    markTerminalMetadataReady(manager, channelId);
  }
}

/** Switch metadata encoding only after the coordinator's capability acknowledgement. */
export function setTerminalMetadataNegotiated(
  manager: SessionManager,
  negotiated: boolean,
): void {
  if (manager.terminalMetadataNegotiated === negotiated) return;
  manager.terminalMetadataNegotiated = negotiated;
  log.info("session-manager", "terminal_metadata_mode", { negotiated });
  if (!negotiated) {
    manager.terminalMetadataReadyRing.clear();
    return;
  }
  for (const channelId of [...manager.rawMetadataQueues.keys()]) {
    disposeRawMetadataState(manager, channelId);
  }
  for (const [channelId, state] of manager.terminalMetadataByChannel) {
    if (state.title !== null) state.titleDirty = true;
    if (state.activityTsMs !== null) state.activityDirty = true;
    markTerminalMetadataReady(manager, channelId);
  }
}

/** Reassert retained title/activity facts once the reconnect snapshot is live. */
export function replayTerminalMetadata(manager: SessionManager): void {
  if (!manager.terminalMetadataNegotiated) return;
  for (const [channelId, state] of manager.terminalMetadataByChannel) {
    if (state.title !== null) state.titleDirty = true;
    if (state.activityTsMs !== null) state.activityDirty = true;
    markTerminalMetadataReady(manager, channelId);
  }
}

/** Retry coalesced metadata after existing transport writable notifications. */
export function flushTerminalMetadata(manager: SessionManager): void {
  if (!manager.terminalMetadataNegotiated || manager.terminalMetadataFlushing) return;
  manager.terminalMetadataFlushing = true;
  try {
    let frames = 0;
    while (frames < TERMINAL_METADATA_DISPATCH_FRAME_BUDGET) {
      const channelId = takeTerminalMetadataReadyChannel(manager);
      if (channelId === null) return;
      const state = manager.terminalMetadataByChannel.get(channelId);
      if (!state || !manager.sessions.has(channelId)) continue;
      const title = state.title;
      const activityTsMs = state.activityTsMs;
      const titleChanged = state.titleDirty && title !== null;
      const activityChanged = state.activityDirty && activityTsMs !== null;
      if (!titleChanged && !activityChanged) continue;
      const send = manager.sendTerminalMetadataUpstream;
      if (!send) {
        manager.terminalMetadataReadyRing.add(channelId);
        return;
      }
      let result: TransportSendResult;
      try {
        result = send({
          channelId,
          titleChanged,
          title: title ?? "",
          activityChanged,
          activityTsMs: activityTsMs ?? 0,
        }) ?? "sent";
      } catch (error) {
        log.warn("session-manager", "terminal_metadata_sink_throw", {
          channel_id: channelId,
          error: error instanceof Error ? error.message : String(error),
        });
        result = "dropped";
      }
      frames += 1;
      if (result === "dropped") {
        manager.terminalMetadataReadyRing.add(channelId);
        return;
      }
      if (titleChanged && state.title === title) state.titleDirty = false;
      if (activityChanged) state.lastActivityPublishedAtMs = Date.now();
      if (activityChanged && state.activityTsMs === activityTsMs) state.activityDirty = false;
      if (
        (state.titleDirty || state.activityDirty)
        && !manager.terminalMetadataReadyRing.has(channelId)
      ) {
        markTerminalMetadataReady(manager, channelId);
      }
    }
  } finally {
    manager.terminalMetadataFlushing = false;
  }
  if (manager.terminalMetadataReadyRing.size > 0) scheduleTerminalMetadataFlush(manager, true);
}

/** Remove all parser and pending metadata state for one terminal channel. */
export function disposeTerminalMetadataState(manager: SessionManager, channelId: number): void {
  manager.terminalMetadataReadyRing.delete(channelId);
  manager.terminalMetadataByChannel.delete(channelId);
}

function markTerminalMetadataReady(manager: SessionManager, channelId: number): void {
  manager.terminalMetadataReadyRing.add(channelId);
  scheduleTerminalMetadataFlush(manager);
}

function takeTerminalMetadataReadyChannel(manager: SessionManager): number | null {
  const next = manager.terminalMetadataReadyRing.values().next().value;
  if (next === undefined) return null;
  manager.terminalMetadataReadyRing.delete(next);
  return next;
}

function scheduleTerminalMetadataFlush(
  manager: SessionManager,
  yieldToEventLoop = false,
): void {
  if (manager.terminalMetadataFlushing) return;
  if (manager.terminalMetadataFlushScheduled && !yieldToEventLoop) return;
  const token = ++manager.terminalMetadataFlushToken;
  manager.terminalMetadataFlushScheduled = true;
  const flush = (): void => {
    if (manager.terminalMetadataFlushToken !== token) return;
    manager.terminalMetadataFlushScheduled = false;
    flushTerminalMetadata(manager);
  };
  if (yieldToEventLoop) {
    // A completed quantum must let reconnect and liveness work run first.
    setTimeout(flush, 0);
  } else {
    queueMicrotask(flush);
  }
}
