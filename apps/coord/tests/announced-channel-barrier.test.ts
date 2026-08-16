import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";
import {
  CoordWorkerUpSchema,
  WCellGridSchema,
  type CoordWorkerUp,
} from "@roost/shared/proto/worker_transport_pb";
import {
  ANNOUNCED_CHANNEL_MAX_FRAMES,
  AnnouncedChannelBarrier,
} from "../src/connect/announced-channel-barrier.ts";

function cell(seq: number, full: boolean): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "cellGrid",
      value: create(WCellGridSchema, {
        channelId: 7,
        frame: create(PbCellGridFrameSchema, {
          sessionId: "00000000-0000-4000-8000-000000000717",
          seq: BigInt(seq),
          full,
          cols: 80,
          rows: 24,
          gridEpoch: "announced",
        }),
      }),
    },
  });
}

test("only an announced channel buffers and drains full before deltas", () => {
  const barrier = new AnnouncedChannelBarrier();
  expect(barrier.enqueue(7, cell(1, true), 100)).toBe("not-announced");
  barrier.announce(7, "session-a");
  expect(barrier.enqueue(7, cell(10, true), 100)).toBe("buffered");
  expect(barrier.enqueue(7, cell(11, false), 100)).toBe("buffered");

  const seen: number[] = [];
  expect(barrier.commit(7, "session-a", () => true, (frame) => {
    if (frame.frame.case === "cellGrid" && frame.frame.value.frame) {
      seen.push(Number(frame.frame.value.frame.seq));
    }
  })).toBe(true);
  expect(seen).toEqual([10, 11]);
  expect(barrier.stats()).toEqual({ channels: 0, frames: 0, bytes: 0 });
});

test("delta-before-full, mapping mismatch, and overflow clear without delivery", () => {
  const barrier = new AnnouncedChannelBarrier();
  barrier.announce(7, "session-a");
  expect(barrier.enqueue(7, cell(2, false), 100)).toBe("dropped");
  expect(barrier.stats().channels).toBe(0);

  barrier.announce(7, "session-a");
  barrier.enqueue(7, cell(10, true), 100);
  let delivered = 0;
  expect(barrier.commit(7, "session-a", () => false, () => { delivered++; })).toBe(false);
  expect(delivered).toBe(0);

  barrier.announce(7, "session-a");
  expect(barrier.enqueue(7, cell(1, true), 1)).toBe("buffered");
  for (let seq = 2; seq <= ANNOUNCED_CHANNEL_MAX_FRAMES; seq++) {
    expect(barrier.enqueue(7, cell(seq, false), 1)).toBe("buffered");
  }
  expect(barrier.enqueue(7, cell(ANNOUNCED_CHANNEL_MAX_FRAMES + 1, false), 1)).toBe("dropped");
  expect(barrier.stats().channels).toBe(0);
});
