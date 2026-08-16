import type { CoordWorkerUp } from "@roost/shared/proto/worker_transport_pb";

export const ANNOUNCED_CHANNEL_MAX_FRAMES = 64;
export const ANNOUNCED_CHANNEL_MAX_BYTES = 4 * 1024 * 1024;
export const ANNOUNCED_CHANNEL_MAX_MS = 3_000;

interface AnnouncedChannel {
  sessionId: string;
  frames: CoordWorkerUp[];
  bytes: number;
  lastSeq: bigint;
  timer: ReturnType<typeof setTimeout>;
}

export type AnnouncedEnqueueResult = "not-announced" | "buffered" | "dropped";

/** Bridges only the worker-open chronology gap: an opened event has been decoded
 * synchronously, but its DB append/channel-map publication is still queued.
 * Unannounced channels never enter this structure. */
export class AnnouncedChannelBarrier {
  private readonly channels = new Map<number, AnnouncedChannel>();

  announce(channelId: number, sessionId: string): void {
    this.fail(channelId);
    const announced: AnnouncedChannel = {
      sessionId,
      frames: [],
      bytes: 0,
      lastSeq: 0n,
      timer: undefined as unknown as ReturnType<typeof setTimeout>,
    };
    announced.timer = setTimeout(() => {
      if (this.channels.get(channelId) === announced) this.channels.delete(channelId);
    }, ANNOUNCED_CHANNEL_MAX_MS);
    announced.timer.unref?.();
    this.channels.set(channelId, announced);
  }

  enqueue(
    channelId: number,
    frame: CoordWorkerUp,
    encodedBytes: number,
  ): AnnouncedEnqueueResult {
    const announced = this.channels.get(channelId);
    if (!announced) return "not-announced";
    if (frame.frame.case !== "cellGrid" || !frame.frame.value.frame) {
      this.fail(channelId);
      return "dropped";
    }
    const cell = frame.frame.value.frame;
    const first = announced.frames.length === 0;
    if (
      (first && !cell.full)
      || (!first && !cell.full && cell.seq !== announced.lastSeq + 1n)
      || announced.frames.length >= ANNOUNCED_CHANNEL_MAX_FRAMES
      || encodedBytes <= 0
      || announced.bytes + encodedBytes > ANNOUNCED_CHANNEL_MAX_BYTES
    ) {
      this.fail(channelId);
      return "dropped";
    }
    announced.frames.push(frame);
    announced.bytes += encodedBytes;
    announced.lastSeq = cell.seq;
    return "buffered";
  }

  /** Commit only after appendEvent returned and the durable opened publication
   * installed the exact worker/channel→session mapping. Delivery is synchronous,
   * so a newer WS callback cannot overtake the retained chain. */
  commit(
    channelId: number,
    sessionId: string,
    mappingMatches: () => boolean,
    deliver: (frame: CoordWorkerUp) => void,
  ): boolean {
    const announced = this.channels.get(channelId);
    if (!announced || announced.sessionId !== sessionId) return false;
    clearTimeout(announced.timer);
    this.channels.delete(channelId);
    if (!mappingMatches()) return false;
    for (const frame of announced.frames) deliver(frame);
    return true;
  }

  fail(channelId: number): void {
    const announced = this.channels.get(channelId);
    if (!announced) return;
    clearTimeout(announced.timer);
    this.channels.delete(channelId);
  }

  clear(): void {
    for (const announced of this.channels.values()) clearTimeout(announced.timer);
    this.channels.clear();
  }

  stats(): { channels: number; frames: number; bytes: number } {
    let frames = 0;
    let bytes = 0;
    for (const announced of this.channels.values()) {
      frames += announced.frames.length;
      bytes += announced.bytes;
    }
    return { channels: this.channels.size, frames, bytes };
  }
}
