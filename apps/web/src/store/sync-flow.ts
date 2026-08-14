import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncClientFrameSchema,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";

export interface SyncFlowSocket {
  readyState: number;
  send: WebSocket["send"];
}

export interface SyncFlowLink {
  ws: SyncFlowSocket;
  accepting: boolean;
}

export function canAcceptSyncLink<Link extends SyncFlowLink>(
  current: Link | null,
  candidate: Link,
  openState: number,
): boolean {
  return current === candidate
    && candidate.accepting
    && candidate.ws.readyState === openState;
}

export function canOpenSyncLink<Link extends SyncFlowLink & { abortReason: unknown }>(
  current: Link | null,
  candidate: Link,
  openState: number,
): boolean {
  return current === candidate
    && candidate.abortReason === null
    && candidate.ws.readyState === openState;
}

export function decodeFirehoseFrame(bytes: Uint8Array): FirehoseFrame {
  return fromBinary(FirehoseFrameSchema, bytes);
}

/**
 * Dispatch a frame that a live callback already accepted, or one retained by
 * the pre-hydration queue. Replay is never generation-gated: reconnecting
 * cannot revoke a frame that was accepted earlier. Only its cumulative ACK is
 * gated to the still-current, accepting, open owner.
 */
export function dispatchSyncFrameCausally<Link extends SyncFlowLink>(
  getCurrent: () => Link | null,
  link: Link,
  openState: number,
  frame: FirehoseFrame,
  dispatch: (frame: FirehoseFrame) => boolean,
): "unapplied" | "dispatched" | "acked" {
  if (!frame.frame.case || !dispatch(frame)) return "unapplied";
  if (!canAcceptSyncLink(getCurrent(), link, openState) || frame.deliverySeq <= 0n) {
    return "dispatched";
  }
  link.ws.send(toBinary(SyncClientFrameSchema, create(SyncClientFrameSchema, {
    ackDeliverySeq: frame.deliverySeq,
  })));
  return "acked";
}

export interface PreHydrationSyncState<Link, Frame> {
  entries: Array<{ link: Link; frame: Frame }>;
  overflowed: boolean;
}

/** Queue until the cap; once overflowed, clear and latch all later drops. */
export function enqueuePreHydrationFrame<Link, Frame>(
  state: PreHydrationSyncState<Link, Frame>,
  entry: { link: Link; frame: Frame },
  cap: number,
): "queued" | "overflow" | "latched" {
  if (state.overflowed) return "latched";
  if (state.entries.length >= cap) {
    state.entries = [];
    state.overflowed = true;
    return "overflow";
  }
  state.entries.push(entry);
  return "queued";
}

/** Drain one ordered snapshot. Return whether dropped frames require a redial. */
export function drainPreHydrationFrames<Link, Frame>(
  state: PreHydrationSyncState<Link, Frame>,
  consume: (link: Link, frame: Frame) => void,
): boolean {
  const entries = state.entries;
  const overflowed = state.overflowed;
  state.entries = [];
  state.overflowed = false;
  for (const entry of entries) consume(entry.link, entry.frame);
  return overflowed;
}

export function isSyncBackpressureClose(code: number, reason: string): boolean {
  return code === 1013 && reason === "sync backpressure";
}

export function isImmediateSyncRedial(reason: string | null): boolean {
  return reason === "visibility"
    || reason === "manual"
    || reason === "stale"
    || reason === "flow";
}

/** Build the one owner for an infinite reconnect loop. */
export function createSingleSyncLoopStarter(start: () => void): () => boolean {
  let started = false;
  return () => {
    if (started) return false;
    started = true;
    start();
    return true;
  };
}
