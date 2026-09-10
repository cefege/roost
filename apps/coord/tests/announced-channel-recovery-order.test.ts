// Covers semantic recovery delivery after a channel barrier has dropped cells.
// A later metadata fact must remain behind the recovered fact while its durable
// route binding drains; this path retains only compact terminal metadata.

import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WBinarySchema,
  WTerminalMetadataSchema,
  type CoordWorkerUp,
} from "@roost/shared/proto/worker_transport_pb";
import {
  ANNOUNCED_CHANNEL_MAX_BYTES,
  AnnouncedChannelBarrier,
} from "../src/connect/announced-channel-barrier.ts";
import { WorkerRetainedWorkBudget } from "../src/connect/worker-frame-queue.ts";

const SESSION = "00000000-0000-4000-8000-000000000717";

function metadata(title: string): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "terminalMetadata",
      value: create(WTerminalMetadataSchema, {
        channelId: 7,
        titleChanged: true,
        title,
        activityChanged: true,
        activityTsMs: 1n,
      }),
    },
  });
}

function overflowBinary(): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "binary",
      value: create(WBinarySchema, {
        channelId: 7,
        direction: 1,
        seq: 1n,
        data: new Uint8Array([0]),
      }),
    },
  });
}

test("mapped metadata during recovery drain follows the recovered fact", async () => {
  const budget = new WorkerRetainedWorkBudget(() => {
    throw new Error("unexpected socket-wide overflow");
  });
  const barrier = new AnnouncedChannelBarrier(undefined, budget);
  barrier.announce(7, SESSION);
  expect(barrier.enqueue(7, metadata("older"), 40)).toBe("buffered");
  expect(barrier.enqueue(
    7, overflowBinary(), ANNOUNCED_CHANNEL_MAX_BYTES,
  )).toBe("dropped");

  const releaseDelivery = Promise.withResolvers<void>();
  const delivered: string[] = [];
  const committing = barrier.commit(7, SESSION, () => true, async (frame) => {
    if (frame.frame.case !== "terminalMetadata") throw new Error("expected metadata");
    delivered.push(frame.frame.value.title);
    if (delivered.length === 1) await releaseDelivery.promise;
  });
  await Promise.resolve();

  expect(barrier.reconcileRetainedMetadata(7, SESSION)).toBe(true);
  expect(barrier.retainUnannouncedMetadata(7, metadata("newer"), 40, SESSION)).toBe(true);
  releaseDelivery.resolve();
  await expect(committing).resolves.toBe(true);
  expect(delivered).toEqual(["older", "newer"]);
  barrier.clear();
});

test("a draining recovery cannot release a replacement session's recovery", async () => {
  const replacementSession = "00000000-0000-4000-8000-000000000718";
  const budget = new WorkerRetainedWorkBudget(() => {
    throw new Error("unexpected socket-wide overflow");
  });
  const barrier = new AnnouncedChannelBarrier(undefined, budget);
  barrier.announce(7, SESSION);
  barrier.enqueue(7, metadata("older"), 40);
  barrier.enqueue(7, overflowBinary(), ANNOUNCED_CHANNEL_MAX_BYTES);

  const releaseDelivery = Promise.withResolvers<void>();
  const firstCommit = barrier.commit(7, SESSION, () => true, async () => {
    await releaseDelivery.promise;
  });
  await Promise.resolve();
  barrier.announce(7, replacementSession);
  barrier.enqueue(7, metadata("replacement"), 40);
  barrier.enqueue(7, overflowBinary(), ANNOUNCED_CHANNEL_MAX_BYTES);
  releaseDelivery.resolve();
  await expect(firstCommit).resolves.toBe(true);

  const delivered: string[] = [];
  await expect(barrier.commit(7, replacementSession, () => true, async (frame) => {
    if (frame.frame.case === "terminalMetadata") delivered.push(frame.frame.value.title);
  })).resolves.toBe(true);
  expect(delivered).toEqual(["replacement"]);
  barrier.clear();
});

test("a pre-commit replacement recovery remains for its replacement session", async () => {
  const replacementSession = "00000000-0000-4000-8000-000000000719";
  const budget = new WorkerRetainedWorkBudget(() => {
    throw new Error("unexpected socket-wide overflow");
  });
  const barrier = new AnnouncedChannelBarrier(undefined, budget);
  barrier.announce(7, SESSION);
  barrier.enqueue(7, metadata("older"), 40);
  barrier.enqueue(7, overflowBinary(), ANNOUNCED_CHANNEL_MAX_BYTES);
  barrier.announce(7, replacementSession);
  barrier.enqueue(7, metadata("replacement"), 40);
  barrier.enqueue(7, overflowBinary(), ANNOUNCED_CHANNEL_MAX_BYTES);

  await expect(barrier.commit(7, SESSION, () => true, async () => undefined)).resolves.toBe(false);
  const delivered: string[] = [];
  await expect(barrier.commit(7, replacementSession, () => true, async (frame) => {
    if (frame.frame.case === "terminalMetadata") delivered.push(frame.frame.value.title);
  })).resolves.toBe(true);
  expect(delivered).toEqual(["replacement"]);
  barrier.clear();
});
