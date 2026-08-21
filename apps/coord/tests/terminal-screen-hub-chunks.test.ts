import { clone, create } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import {
  CELL_GRID_CHUNK_STALL_MS,
  CELL_GRID_PART_MAX_BYTES,
  CELL_GRID_SNAPSHOT_MAX_CHUNKS,
} from "@roost/shared/cell";
import {
  PbCellGridChunkSchema,
  PbCellGridFrameSchema,
  PbCellRowSchema,
} from "@roost/shared/proto/cell_pb";
import {
  SESSION,
  SNAPSHOT_A,
  STREAM,
  chunks,
  fullFrame,
  makeHarness,
} from "./terminal-screen-hub-harness.ts";

describe("TerminalScreenHub bounded chunk assembly", () => {
  test("latches out-of-order, missing-row, and chunk-count failures once", () => {
    const order = makeHarness();
    order.hub.expectStream(SESSION, STREAM, 8, 2);
    const orderSource = fullFrame();
    const ordered = chunks(orderSource, [
      [orderSource.viewportRows[0]!],
      [orderSource.viewportRows[1]!],
    ]);
    order.hub.publishChunk(SESSION, ordered[1]!);
    order.hub.publishChunk(SESSION, ordered[1]!);
    expect(order.requests).toEqual([[SESSION, STREAM]]);
    expect(order.hub.snapshot(SESSION)).toBeNull();

    const missing = makeHarness();
    missing.hub.expectStream(SESSION, STREAM, 8, 2);
    const missingSource = fullFrame();
    const missingRow = chunks(missingSource, [[missingSource.viewportRows[0]!]])[0]!;
    missing.hub.publishChunk(SESSION, missingRow);
    missing.hub.publishChunk(SESSION, missingRow);
    expect(missing.requests).toEqual([[SESSION, STREAM]]);
    expect(missing.hub.snapshot(SESSION)).toBeNull();

    const capped = makeHarness();
    capped.hub.expectStream(SESSION, STREAM, 8, 2);
    const capSource = fullFrame();
    const capPart = clone(PbCellGridFrameSchema, capSource);
    capPart.viewportRows = [clone(PbCellRowSchema, capSource.viewportRows[0]!)];
    const overCap = create(PbCellGridChunkSchema, {
      snapshotId: SNAPSHOT_A,
      chunkIndex: 0,
      chunkCount: CELL_GRID_SNAPSHOT_MAX_CHUNKS + 1,
      part: capPart,
    });
    capped.hub.publishChunk(SESSION, overCap);
    capped.hub.publishChunk(SESSION, overCap);
    expect(capped.requests).toEqual([[SESSION, STREAM]]);
    expect(capped.hub.snapshot(SESSION)).toBeNull();
  });

  test("rejects an oversized unchunked baseline without exposing a cache", () => {
    const { hub, requests } = makeHarness();
    hub.expectStream(SESSION, STREAM, 1, 1);
    hub.publishFrame(SESSION, fullFrame({
      cols: 1,
      rows: 1,
      texts: ["x".repeat(CELL_GRID_PART_MAX_BYTES)],
    }));
    expect(requests).toEqual([[SESSION, STREAM]]);
    expect(hub.snapshot(SESSION)).toBeNull();
  });

  test("expires a partial at the exact stall boundary and keeps the latch closed", () => {
    const clock = { value: 0 };
    const { hub, requests, timers, fireTimer } = makeHarness(clock);
    hub.expectStream(SESSION, STREAM, 8, 2);
    const source = fullFrame();
    const partial = chunks(source, [
      [source.viewportRows[0]!],
      [source.viewportRows[1]!],
    ]);

    hub.publishChunk(SESSION, partial[0]!);
    expect(hub.snapshot(SESSION)).toBeNull();
    expect(timers.size).toBe(1);
    const [timerId, timer] = [...timers.entries()][0]!;
    expect(timer.delayMs).toBe(CELL_GRID_CHUNK_STALL_MS);

    clock.value = CELL_GRID_CHUNK_STALL_MS;
    fireTimer(timerId);
    expect(requests).toEqual([[SESSION, STREAM]]);
    expect(hub.snapshot(SESSION)).toBeNull();

    hub.publishChunk(SESSION, partial[1]!);
    expect(requests).toEqual([[SESSION, STREAM]]);
    expect(hub.snapshot(SESSION)).toBeNull();
  });
});
