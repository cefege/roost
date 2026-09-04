// Encapsulates Sync v2 queue ordering and weighted-lane candidate selection.
// These mutations preserve each domain's snapshot boundary while allowing an
// eligible opened event to pass its fenced terminal cells, and enforce the age
// escape hatch without coupling policy to socket writes.

import type { ServerWebSocket } from "bun";
import { SyncDomain, type FirehoseFrame } from "@roost/shared/proto/sync_pb";
import type { SyncWsData } from "./sync-ws-handler.ts";
import type { WsDeadlineClock } from "./ws-auth-deadline.ts";
import {
  V2_LOW_LANE_MAX_AGE_MS,
  V2_WEIGHTED_LANES,
  isV2SnapshotFrame,
  queuedV2FrameEligible,
  releaseV2AggregateFrame,
  type SyncV2DomainState,
  type SyncV2QueuedFrame,
} from "./sync-ws-v2-state.ts";

function isTerminalCellFrame(frame: FirehoseFrame): boolean {
  return frame.frame.case === "cellGrid" || frame.frame.case === "cellGridChunk";
}

// A freshly attached session's baseline may pass other sessions' queued deltas,
// but never this session's own queued frames or another session's snapshot.
export function v2AttachSnapshotInsertIndex(
  queue: readonly SyncV2QueuedFrame[],
  sessionId: string | undefined,
): number {
  let insertIndex = queue.length;
  for (let index = queue.length - 1; index >= 0; index -= 1) {
    const item = queue[index]!;
    if (
      (item.meta.sessionId !== undefined && item.meta.sessionId === sessionId)
      || isV2SnapshotFrame(item.frame)
    ) {
      return index + 1;
    }
    insertIndex = index;
  }
  return insertIndex;
}

interface SyncV2Candidate {
  domain: SyncV2DomainState;
  index: number;
  item: SyncV2QueuedFrame;
}

export function selectV2Candidate(
  ws: ServerWebSocket<SyncWsData>,
  deadlineClock: WsDeadlineClock,
): SyncV2Candidate | null {
  const v2 = ws.data.v2;
  if (!v2) return null;
  const heads: SyncV2Candidate[] = [];
  // We may step past an ineligible fenced cell to reach its opened event, but
  // never reorder two eligible items from the same domain.
  for (const domain of v2.domains.values()) {
    if (!domain.subscribed || !domain.ready) continue;
    for (let index = 0; index < domain.queue.length; index += 1) {
      const item = domain.queue[index]!;
      if (!queuedV2FrameEligible(v2, ws.data.ackDeliverySeq, item)) continue;
      heads.push({ domain, index, item });
      break;
    }
  }
  if (heads.length === 0) return null;

  const nowMs = deadlineClock.now();
  let overdue: SyncV2Candidate | null = null;
  for (const candidate of heads) {
    if (
      candidate.item.meta.lane !== "cell"
      && nowMs - candidate.item.queuedAtMs >= V2_LOW_LANE_MAX_AGE_MS
      && (!overdue || candidate.item.queuedAtMs < overdue.item.queuedAtMs)
    ) overdue = candidate;
  }
  if (overdue) return overdue;

  for (let attempt = 0; attempt < V2_WEIGHTED_LANES.length; attempt += 1) {
    const lane = V2_WEIGHTED_LANES[v2.laneCursor]!;
    v2.laneCursor = (v2.laneCursor + 1) % V2_WEIGHTED_LANES.length;
    let selected: SyncV2Candidate | null = null;
    for (const candidate of heads) {
      if (candidate.item.meta.lane !== lane) continue;
      if (!selected || candidate.item.queuedAtMs < selected.item.queuedAtMs) {
        selected = candidate;
      }
    }
    if (selected) return selected;
  }
  return heads.reduce((oldest, candidate) =>
    candidate.item.queuedAtMs < oldest.item.queuedAtMs ? candidate : oldest
  );
}

export function removeTerminalQueued(
  ws: ServerWebSocket<SyncWsData>,
  sessionId: string,
  includeState: boolean,
): void {
  const v2 = ws.data.v2;
  const terminal = v2?.domains.get(SyncDomain.TERMINAL);
  if (!v2 || !terminal) return;
  for (let index = terminal.queue.length - 1; index >= 0; index--) {
    const item = terminal.queue[index]!;
    if (item.meta.sessionId !== sessionId || item.meta.lane !== "cell") continue;
    if (!includeState && !isTerminalCellFrame(item.frame)) continue;
    if (index < terminal.seedInsertIndex) terminal.seedInsertIndex--;
    terminal.queue.splice(index, 1);
    terminal.queuedBytes -= item.estimatedBytes;
    releaseV2AggregateFrame(v2, item);
  }
}
