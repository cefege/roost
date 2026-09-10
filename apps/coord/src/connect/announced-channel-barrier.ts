// Bridges worker-open frames until their durable channel route publishes.
// Cell loss invalidates the stream; one compact semantic record survives until
// the exact route commits. Every retained frame consumes the socket work budget.
import type { CoordWorkerUp } from "@roost/shared/proto/worker_transport_pb";
import {
  AnnouncedChannelSemanticRetention,
  isCompactTerminalMetadataFrame,
  mergeTerminalMetadataFrames,
  type RetainedTerminalMetadataFrame,
} from "./announced-channel-semantic-retention.ts";
import type { WorkerRetainedWorkBudget } from "./worker-frame-queue.ts";
export const ANNOUNCED_CHANNEL_MAX_FRAMES = 64;
export const ANNOUNCED_CHANNEL_MAX_BYTES = 4 * 1024 * 1024;
export const ANNOUNCED_CHANNEL_MAX_MS = 3_000;
export type AnnouncedDropReason =
  | "overflow"
  | "timeout"
  | "out_of_order"
  | "mapping_mismatch"
  | "superseded"
  | "append_failed"
  | "publish_failed";
export interface AnnouncedDrop {
  channelId: number;
  sessionId: string;
  reason: AnnouncedDropReason;
  phase: AnnouncedPhase;
  cellFrames: number;
  metadataFrames: number;
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
  preAnnouncedMetadata: number;
  recoveryMetadata: number;
}
type Timer = NodeJS.Timeout;
interface RetainedFrame {
  encodedBytes: number;
  retained: boolean;
}
interface BufferedFrame extends RetainedFrame {
  frame: CoordWorkerUp;
  cell: boolean;
  metadata: boolean;
  binaryBytes: number;
  delivering: boolean;
}
interface AnnouncedChannel {
  sessionId: string;
  phase: AnnouncedPhase;
  buffered: BufferedFrame[];
  bytes: number;
  cellFrames: number;
  metadataFrames: number;
  binaryFrames: number;
  binaryBytes: number;
  sawCellFrame: boolean;
  lastCellSeq: bigint;
  metadata: BufferedFrame | null;
  timer: Timer;
}
/** Holds only the short pre-publication interval. A cell-loss drop retains its
 * latest semantic state only until the matching durable route finishes. */
export class AnnouncedChannelBarrier {
  private readonly channels = new Map<number, AnnouncedChannel>();
  private readonly semanticRetention: AnnouncedChannelSemanticRetention;
  private readonly onDrop: ((drop: AnnouncedDrop) => void) | undefined;
  private retainedWorkBudget: WorkerRetainedWorkBudget | null;
  constructor(
    onDrop?: (drop: AnnouncedDrop) => void,
    retainedWorkBudget?: WorkerRetainedWorkBudget,
  ) {
    this.onDrop = onDrop;
    this.retainedWorkBudget = retainedWorkBudget ?? null;
    this.semanticRetention = new AnnouncedChannelSemanticRetention(retainedWorkBudget);
  }
  bindRetainedWorkBudget(retainedWorkBudget: WorkerRetainedWorkBudget): void {
    if (this.retainedWorkBudget === retainedWorkBudget) return;
    if (this.retainedWorkBudget || this.channels.size !== 0) {
      throw new Error("announced-channel barrier budget already bound");
    }
    this.retainedWorkBudget = retainedWorkBudget;
    this.semanticRetention.bindRetainedWorkBudget(retainedWorkBudget);
  }
  announce(channelId: number, sessionId: string): void {
    this.fail(channelId, "superseded");
    // A same-session retry carries its compact recovery into the new barrier.
    const recoveredMetadata = this.semanticRetention.takeRecoveryForSession(
      channelId,
      sessionId,
    );
    const announced: AnnouncedChannel = {
      sessionId,
      phase: "pending",
      buffered: [],
      bytes: 0,
      cellFrames: 0,
      metadataFrames: 0,
      binaryFrames: 0,
      binaryBytes: 0,
      sawCellFrame: false,
      lastCellSeq: 0n,
      metadata: null,
      timer: undefined as unknown as Timer,
    };
    const earlyMetadata = this.semanticRetention.takePreAnnounced(channelId);
    if (recoveredMetadata) this.appendRetainedMetadata(announced, recoveredMetadata);
    if (earlyMetadata) this.appendRetainedMetadata(announced, earlyMetadata);
    announced.timer = setTimeout(() => {
      if (this.channels.get(channelId) === announced) this.drop(channelId, announced, "timeout");
    }, ANNOUNCED_CHANNEL_MAX_MS);
    announced.timer.unref?.();
    this.channels.set(channelId, announced);
  }
  isAnnounced(channelId: number): boolean {
    return this.channels.has(channelId);
  }
  reconcileRetainedMetadata(channelId: number, sessionId: string): boolean {
    return this.semanticRetention.reconcileMappedRoute(channelId, sessionId);
  }
  retainUnannouncedMetadata(
    channelId: number,
    frame: CoordWorkerUp,
    encodedBytes: number,
    routeSessionId: string | null = null,
  ): boolean {
    return this.semanticRetention.retainUnannounced(
      channelId, frame, encodedBytes, routeSessionId,
    );
  }
  enqueue(
    channelId: number,
    frame: CoordWorkerUp,
    encodedBytes: number,
  ): AnnouncedEnqueueResult {
    const announced = this.channels.get(channelId);
    if (!announced) return "not-announced";
    const cell = frame.frame.case === "cellGrid"
      ? frame.frame.value.frame ?? null
      : frame.frame.case === "cellGridChunk"
        ? frame.frame.value.chunk?.part ?? null
        : null;
    const binary = frame.frame.case === "binary" ? frame.frame.value : null;
    const metadata = frame.frame.case === "terminalMetadata" ? frame.frame.value : null;
    if (!cell && !binary && !metadata) {
      this.drop(channelId, announced, "out_of_order");
      return "dropped";
    }
    const replacementMetadata = metadata && announced.metadata && !announced.metadata.delivering
      ? announced.metadata
      : null;
    const previousMetadataBytes = replacementMetadata?.encodedBytes ?? 0;
    const binaryBytes = binary?.data.byteLength ?? 0;
    const rejected = {
      cellFrames: Number(cell !== null),
      metadataFrames: Number(metadata !== null),
      binaryFrames: Number(binary !== null),
      binaryBytes,
    };
    if (
      !Number.isSafeInteger(encodedBytes)
      || encodedBytes <= 0
      || (metadata !== null && !isCompactTerminalMetadataFrame(frame, encodedBytes))
    ) {
      this.drop(channelId, announced, "overflow", rejected);
      return "dropped";
    }
    const retainedMetadata = replacementMetadata
      ? mergeTerminalMetadataFrames(replacementMetadata.frame, frame)
      : { frame, encodedBytes };
    const bufferedFrames = announced.buffered.length - Number(replacementMetadata !== null);
    const bufferedBytes = announced.bytes - previousMetadataBytes;
    if (
      (metadata !== null && !isCompactTerminalMetadataFrame(
        retainedMetadata.frame,
        retainedMetadata.encodedBytes,
      ))
      || bufferedFrames >= ANNOUNCED_CHANNEL_MAX_FRAMES
      || bufferedBytes + retainedMetadata.encodedBytes > ANNOUNCED_CHANNEL_MAX_BYTES
    ) {
      this.drop(channelId, announced, "overflow", rejected);
      return "dropped";
    }
    if (
      cell
      && !cell.full
      && (!announced.sawCellFrame || cell.seq !== announced.lastCellSeq + 1n)
    ) {
      this.drop(channelId, announced, "out_of_order", rejected);
      return "dropped";
    }
    const retainedWorkBudget = this.retainedWorkBudget;
    if (!retainedWorkBudget) throw new Error("announced-channel barrier budget is not bound");
    if (replacementMetadata) this.releaseRetained(replacementMetadata);
    if (retainedWorkBudget.retain(retainedMetadata.encodedBytes) !== "retained") {
      if (replacementMetadata) announced.metadata = null;
      this.drop(channelId, announced, "overflow", rejected);
      return "dropped";
    }
    if (replacementMetadata) {
      replacementMetadata.frame = retainedMetadata.frame;
      replacementMetadata.encodedBytes = retainedMetadata.encodedBytes;
      replacementMetadata.retained = true;
      announced.bytes += retainedMetadata.encodedBytes - previousMetadataBytes;
      return "buffered";
    }
    const buffered: BufferedFrame = {
      frame: retainedMetadata.frame,
      encodedBytes: retainedMetadata.encodedBytes,
      cell: cell !== null,
      metadata: metadata !== null,
      binaryBytes,
      retained: true,
      delivering: false,
    };
    announced.buffered.push(buffered);
    announced.bytes += buffered.encodedBytes;
    if (cell) {
      announced.sawCellFrame = true;
      announced.lastCellSeq = cell.seq;
      announced.cellFrames += 1;
    } else if (metadata) {
      announced.metadataFrames += 1;
      announced.metadata = buffered;
    } else {
      announced.binaryFrames += 1;
      announced.binaryBytes += binaryBytes;
    }
    return "buffered";
  }
  async commit(
    channelId: number,
    sessionId: string,
    mappingMatches: () => boolean,
    deliver: (frame: CoordWorkerUp) => Promise<void>,
  ): Promise<boolean> {
    const announced = this.channels.get(channelId);
    if (!announced) return this.semanticRetention.commitRecovery(
      channelId, sessionId, mappingMatches, deliver,
    );
    if (announced.sessionId !== sessionId) return false;
    if (!mappingMatches()) {
      this.drop(channelId, announced, "mapping_mismatch");
      return false;
    }
    announced.phase = "draining";
    while (announced.buffered.length > 0) {
      if (this.channels.get(channelId) !== announced) return false;
      const next = announced.buffered[0]!;
      next.delivering = true;
      let failure: unknown;
      try {
        await deliver(next.frame);
      } catch (error) {
        failure = error;
      } finally {
        this.releaseRetained(next);
      }
      if (this.channels.get(channelId) !== announced) {
        if (failure) throw failure;
        return false;
      }
      announced.buffered.shift();
      announced.bytes -= next.encodedBytes;
      if (next.cell) announced.cellFrames -= 1;
      else if (next.metadata) {
        announced.metadataFrames -= 1;
        if (announced.metadata === next) announced.metadata = null;
      } else {
        announced.binaryFrames -= 1;
        announced.binaryBytes -= next.binaryBytes;
      }
      if (failure) {
        this.drop(channelId, announced, "publish_failed");
        throw failure;
      }
    }
    if (this.channels.get(channelId) !== announced) return false;
    clearTimeout(announced.timer);
    this.channels.delete(channelId);
    return true;
  }
  fail(channelId: number, reason: AnnouncedDropReason = "append_failed"): void {
    const announced = this.channels.get(channelId);
    if (announced) this.drop(channelId, announced, reason);
    else if (reason === "append_failed") this.semanticRetention.discardRecovery(channelId);
  }
  clear(): void {
    for (const announced of this.channels.values()) {
      clearTimeout(announced.timer);
      for (const frame of announced.buffered) {
        if (!frame.delivering) this.releaseRetained(frame);
      }
      announced.buffered.length = 0;
    }
    this.channels.clear();
    this.semanticRetention.clear();
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
    const semantic = this.semanticRetention.stats();
    return {
      channels: this.channels.size,
      frames: frames + semantic.frames,
      bytes: bytes + semantic.bytes,
      pending,
      draining,
      preAnnouncedMetadata: semantic.preAnnounced,
      recoveryMetadata: semantic.recovery,
    };
  }
  private appendRetainedMetadata(
    announced: AnnouncedChannel,
    metadata: RetainedTerminalMetadataFrame,
  ): void {
    const buffered: BufferedFrame = {
      frame: metadata.frame,
      encodedBytes: metadata.encodedBytes,
      cell: false,
      metadata: true,
      binaryBytes: 0,
      retained: metadata.retained,
      delivering: false,
    };
    announced.buffered.push(buffered);
    announced.bytes += buffered.encodedBytes;
    announced.metadataFrames += 1;
    announced.metadata = buffered;
  }
  private drop(
    channelId: number,
    announced: AnnouncedChannel,
    reason: AnnouncedDropReason,
    rejected = { cellFrames: 0, metadataFrames: 0, binaryFrames: 0, binaryBytes: 0 },
  ): void {
    clearTimeout(announced.timer);
    if (this.channels.get(channelId) === announced) this.channels.delete(channelId);
    const retainedMetadata = announced.phase === "pending"
      && (reason === "overflow" || reason === "timeout" || reason === "out_of_order")
      ? announced.metadata
      : null;
    const drop: AnnouncedDrop = {
      channelId,
      sessionId: announced.sessionId,
      reason,
      phase: announced.phase,
      cellFrames: announced.cellFrames + rejected.cellFrames,
      metadataFrames: announced.metadataFrames + rejected.metadataFrames,
      binaryFrames: announced.binaryFrames + rejected.binaryFrames,
      binaryBytes: announced.binaryBytes + rejected.binaryBytes,
    };
    for (const frame of announced.buffered) {
      if (frame !== retainedMetadata && !frame.delivering) this.releaseRetained(frame);
    }
    announced.buffered.length = 0;
    announced.bytes = 0;
    announced.cellFrames = 0;
    announced.metadataFrames = 0;
    announced.binaryFrames = 0;
    announced.binaryBytes = 0;
    announced.metadata = null;
    if (retainedMetadata) {
      this.semanticRetention.parkRecovery(channelId, announced.sessionId, retainedMetadata);
    }
    this.onDrop?.(drop);
  }
  private releaseRetained(frame: RetainedFrame): void {
    if (!frame.retained) return;
    frame.retained = false;
    this.retainedWorkBudget!.release(frame.encodedBytes);
  }
}
