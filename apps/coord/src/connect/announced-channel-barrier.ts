import type { CoordWorkerUp } from "@roost/shared/proto/worker_transport_pb";

export const ANNOUNCED_CHANNEL_MAX_FRAMES = 64;
export const ANNOUNCED_CHANNEL_MAX_BYTES = 4 * 1024 * 1024;
export const ANNOUNCED_CHANNEL_MAX_MS = 3_000;

/** Why an announced channel abandoned its buffer. Cells and PTY bytes are lost
 * differently: a later full frame recreates the grid, but a one-time OSC 0/2
 * title only ever crosses the binary lane once. */
export type AnnouncedDropReason =
  | "overflow"
  | "timeout"
  | "out_of_order"
  | "mapping_mismatch"
  | "superseded"
  | "append_failed"
  | "publish_failed";

/** Everything still unpublished when the barrier gave up, plus the phase that
 * decides how much is at risk: a `pending` channel has no committed route yet,
 * so cells arriving after the drop are dropped as unmapped too. */
export interface AnnouncedDrop {
  channelId: number;
  sessionId: string;
  reason: AnnouncedDropReason;
  phase: AnnouncedPhase;
  cellFrames: number;
  binaryFrames: number;
  binaryBytes: number;
}

export type AnnouncedPhase = "pending" | "draining";

export type AnnouncedEnqueueResult = "not-announced" | "buffered" | "dropped";

export interface AnnouncedBarrierStats {
  channels: number;
  frames: number;
  bytes: number;
  pending: number;
  draining: number;
}

interface BufferedFrame {
  frame: CoordWorkerUp;
  encodedBytes: number;
  cell: boolean;
  binaryBytes: number;
}

interface AnnouncedChannel {
  sessionId: string;
  phase: AnnouncedPhase;
  buffered: BufferedFrame[];
  bytes: number;
  cellFrames: number;
  binaryFrames: number;
  binaryBytes: number;
  sawCellFrame: boolean;
  lastCellSeq: bigint;
  timer: Timer;
}

/** Bridges only the worker-open chronology gap: an `opened`/`respawned` event
 * has been decoded synchronously, but its DB append and channel-map publication
 * are still queued. Both fan-out lanes — cell grids and raw PTY binary — buffer
 * here in their exact arrival order, because a respawn's first binary frame can
 * carry the only copy of its title/OSC mapping and must publish after the
 * durable commit yet still ahead of the cell frames that followed it.
 * Unannounced channels never enter this structure. */
export class AnnouncedChannelBarrier {
  private readonly channels = new Map<number, AnnouncedChannel>();
  private readonly onDrop: ((drop: AnnouncedDrop) => void) | undefined;

  constructor(onDrop?: (drop: AnnouncedDrop) => void) {
    this.onDrop = onDrop;
  }

  announce(channelId: number, sessionId: string): void {
    // A second announcement before the first committed means the earlier
    // buffer can never bind its route: report it instead of silently freeing it.
    this.fail(channelId, "superseded");
    const announced: AnnouncedChannel = {
      sessionId,
      phase: "pending",
      buffered: [],
      bytes: 0,
      cellFrames: 0,
      binaryFrames: 0,
      binaryBytes: 0,
      sawCellFrame: false,
      lastCellSeq: 0n,
      timer: undefined as unknown as Timer,
    };
    // One absolute deadline from announcement covers both phases: a durable
    // append that never commits and a drain that stops making progress.
    announced.timer = setTimeout(() => {
      if (this.channels.get(channelId) === announced) this.drop(channelId, announced, "timeout");
    }, ANNOUNCED_CHANNEL_MAX_MS);
    announced.timer.unref?.();
    this.channels.set(channelId, announced);
  }

  /** Cheap pre-check for the fan-out hot path: measuring a frame means encoding
   *  it, which is affordable only inside a channel's short announcement window —
   *  the steady-state PTY/cell lanes must never re-serialize. */
  isAnnounced(channelId: number): boolean {
    return this.channels.has(channelId);
  }

  enqueue(
    channelId: number,
    frame: CoordWorkerUp,
    encodedBytes: number,
  ): AnnouncedEnqueueResult {
    const announced = this.channels.get(channelId);
    if (!announced) return "not-announced";
    const cell = frame.frame.case === "cellGrid" ? frame.frame.value.frame ?? null : null;
    const binary = frame.frame.case === "binary" ? frame.frame.value : null;
    // Only the two unordered fan-out lanes may cross a barrier. Anything else
    // belongs on the serialized event tail, so treat it as a lost ordering.
    if (!cell && !binary) {
      this.drop(channelId, announced, "out_of_order");
      return "dropped";
    }
    const binaryBytes = binary?.data.byteLength ?? 0;
    if (
      encodedBytes <= 0
      || announced.buffered.length >= ANNOUNCED_CHANNEL_MAX_FRAMES
      || announced.bytes + encodedBytes > ANNOUNCED_CHANNEL_MAX_BYTES
    ) {
      this.drop(channelId, announced, "overflow", {
        cellFrames: cell ? 1 : 0,
        binaryFrames: binary ? 1 : 0,
        binaryBytes,
      });
      return "dropped";
    }
    // Cell continuity is checked across the cell frames alone: a binary frame
    // legitimately precedes the channel's first full grid.
    if (cell) {
      const ordered = cell.full
        || (announced.sawCellFrame && cell.seq === announced.lastCellSeq + 1n);
      if (!ordered) {
        this.drop(channelId, announced, "out_of_order", { cellFrames: 1, binaryFrames: 0, binaryBytes: 0 });
        return "dropped";
      }
      announced.sawCellFrame = true;
      announced.lastCellSeq = cell.seq;
      announced.cellFrames += 1;
    } else {
      announced.binaryFrames += 1;
      announced.binaryBytes += binaryBytes;
    }
    announced.buffered.push({ frame, encodedBytes, cell: cell !== null, binaryBytes });
    announced.bytes += encodedBytes;
    return "buffered";
  }

  /** Commit only after appendEvent returned and the durable publication
   * installed the exact worker/channel→session mapping. The buffer then drains
   * on the caller's socket lane: each frame is awaited in arrival order and
   * arrivals during the drain join the same tail, so no later fast-path frame
   * can overtake the channel's first frames. The channel is marked open — its
   * entry removed — only once the buffer is empty. */
  async commit(
    channelId: number,
    sessionId: string,
    mappingMatches: () => boolean,
    deliver: (frame: CoordWorkerUp) => Promise<void>,
  ): Promise<boolean> {
    const announced = this.channels.get(channelId);
    if (!announced || announced.sessionId !== sessionId) return false;
    if (!mappingMatches()) {
      this.drop(channelId, announced, "mapping_mismatch");
      return false;
    }
    announced.phase = "draining";
    while (announced.buffered.length > 0) {
      // A timeout, an overflow from a concurrent arrival, or a replacement
      // announcement can retire this entry mid-drain; its own drop reported the
      // remaining loss, so stop rather than publishing past it.
      if (this.channels.get(channelId) !== announced) return false;
      const next = announced.buffered.shift()!;
      announced.bytes -= next.encodedBytes;
      if (next.cell) announced.cellFrames -= 1;
      else {
        announced.binaryFrames -= 1;
        announced.binaryBytes -= next.binaryBytes;
      }
      try {
        await deliver(next.frame);
      } catch (error) {
        this.drop(channelId, announced, "publish_failed");
        throw error;
      }
    }
    if (this.channels.get(channelId) !== announced) return false;
    clearTimeout(announced.timer);
    this.channels.delete(channelId);
    return true;
  }

  fail(channelId: number, reason: AnnouncedDropReason = "append_failed"): void {
    const announced = this.channels.get(channelId);
    if (!announced) return;
    this.drop(channelId, announced, reason);
  }

  /** Socket teardown. The returning worker's reconcile snapshot is a producer
   * generation change that already forces a full frame for every active owner,
   * so a dying route needs no per-channel loss report. */
  clear(): void {
    for (const announced of this.channels.values()) clearTimeout(announced.timer);
    this.channels.clear();
  }

  stats(): AnnouncedBarrierStats {
    let frames = 0;
    let bytes = 0;
    let pending = 0;
    let draining = 0;
    for (const announced of this.channels.values()) {
      frames += announced.buffered.length;
      bytes += announced.bytes;
      if (announced.phase === "pending") pending += 1;
      else draining += 1;
    }
    return { channels: this.channels.size, frames, bytes, pending, draining };
  }

  private drop(
    channelId: number,
    announced: AnnouncedChannel,
    reason: AnnouncedDropReason,
    rejected: { cellFrames: number; binaryFrames: number; binaryBytes: number } = {
      cellFrames: 0,
      binaryFrames: 0,
      binaryBytes: 0,
    },
  ): void {
    clearTimeout(announced.timer);
    if (this.channels.get(channelId) === announced) this.channels.delete(channelId);
    const drop: AnnouncedDrop = {
      channelId,
      sessionId: announced.sessionId,
      reason,
      phase: announced.phase,
      cellFrames: announced.cellFrames + rejected.cellFrames,
      binaryFrames: announced.binaryFrames + rejected.binaryFrames,
      binaryBytes: announced.binaryBytes + rejected.binaryBytes,
    };
    announced.buffered.length = 0;
    announced.bytes = 0;
    announced.cellFrames = 0;
    announced.binaryFrames = 0;
    announced.binaryBytes = 0;
    this.onDrop?.(drop);
  }
}
