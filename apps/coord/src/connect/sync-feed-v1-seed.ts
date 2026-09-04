// Sync v1 retained seeding buffers live frames while ACK-paced snapshots drain.
// startSyncFeed owns subscriptions and feed disposal; this leaf owns the bounded
// live queue, exact byte accounting, and FIFO handoff to paced delivery.

import { toBinary } from "@bufbuild/protobuf";
import { FirehoseFrameSchema, type FirehoseFrame } from "@roost/shared/proto/sync_pb";
import { log } from "@roost/shared/log";
import type { SyncFeedFrameMeta } from "./sync-feed-frames.ts";

export interface SyncFeedSeedOptions {
  readonly version?: 1;
  pacedSeedPush: (frame: FirehoseFrame) => Promise<boolean>;
  onBufferOverflow: (reason: "frame_limit" | "byte_limit", frame: string) => void;
}

interface QueuedLiveFrame {
  readonly frame: FirehoseFrame;
  readonly encodedBytes: number;
}

export interface SyncFeedV1SeedDelivery {
  readonly seeded: Promise<void>;
  push(frame: FirehoseFrame, meta: SyncFeedFrameMeta): void;
  dispose(): void;
}

export function createSyncFeedV1SeedDelivery(
  options: SyncFeedSeedOptions,
  retainedFrames: Iterable<FirehoseFrame>,
  sink: (frame: FirehoseFrame, meta?: SyncFeedFrameMeta) => void,
  maxQueuedFrames: number,
  maxQueuedBytes: number,
): SyncFeedV1SeedDelivery {
  let disposed = false;
  let seeding = true;
  const queuedLiveFrames: QueuedLiveFrame[] = [];
  let queuedLiveBytes = 0;

  const clearQueuedLive = (): void => {
    queuedLiveFrames.length = 0;
    queuedLiveBytes = 0;
  };
  const push = (frame: FirehoseFrame, meta: SyncFeedFrameMeta): void => {
    if (disposed) return;
    if (!seeding) return sink(frame, meta);
    const encodedBytes = toBinary(FirehoseFrameSchema, frame).byteLength;
    const overflowReason = queuedLiveFrames.length >= maxQueuedFrames
      ? "frame_limit"
      : encodedBytes > maxQueuedBytes - queuedLiveBytes
        ? "byte_limit"
        : null;
    if (overflowReason) {
      options.onBufferOverflow(overflowReason, frame.frame.case ?? "unknown");
      return;
    }
    queuedLiveFrames.push({ frame, encodedBytes });
    queuedLiveBytes += encodedBytes;
  };
  const seeded = Promise.resolve().then(async () => {
    for (const frame of retainedFrames) {
      if (disposed || !(await options.pacedSeedPush(frame))) return;
    }
    while (!disposed) {
      const entry = queuedLiveFrames[0];
      if (!entry) {
        seeding = false;
        clearQueuedLive();
        return;
      }
      if (!(await options.pacedSeedPush(entry.frame))) return;
      queuedLiveFrames.shift();
      queuedLiveBytes -= entry.encodedBytes;
    }
  }).catch((error) => {
    log.warn("connect.sync", "retained_seed_failed", { error: String(error) });
  });

  return {
    seeded,
    push,
    dispose: () => {
      disposed = true;
      seeding = false;
      clearQueuedLive();
    },
  };
}
