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
  OTHER_STREAM,
  SNAPSHOT_B,
  STREAM,
  TestSink,
  chunks,
  deltaFrame,
  fullFrame,
  makeHarness,
  seededFrame,
  texts,
  watch,
} from "./terminal-screen-hub-harness.ts";
import { TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_FRAMES } from "../src/connect/terminal-screen-hub-state.ts";
import type { FirehoseFrame } from "@roost/shared/proto/sync_pb";

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

  test("redrives a latched request when a partial stalls at the exact boundary", () => {
    const clock = { value: 0 };
    const { hub, requests, timers, fireTimer } = makeHarness(clock);
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, deltaFrame());
    expect(requests).toEqual([[SESSION, STREAM]]);
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
    expect(requests).toEqual([
      [SESSION, STREAM],
      [SESSION, STREAM],
    ]);
    expect(hub.snapshot(SESSION)).toBeNull();

    hub.publishChunk(SESSION, partial[1]!);
    expect(requests).toHaveLength(2);
    expect(hub.snapshot(SESSION)).toBeNull();
  });
});

function outboundTexts(frame: FirehoseFrame): string[] {
  if (frame.frame.case !== "cellGrid") throw new Error("expected a cell grid frame");
  return texts(frame.frame.value);
}

describe("TerminalScreenHub delta hold during assembly", () => {
  test("holds live deltas during chunk assembly and folds them like an uninterrupted run", () => {
    const held = makeHarness();
    const heldSink = new TestSink();
    watch(held.hub, heldSink);
    held.hub.expectStream(SESSION, STREAM, 8, 2);
    held.hub.publishFrame(SESSION, fullFrame({ texts: ["old-0", "old-1"] }));

    const replacement = fullFrame({ seq: 3n, texts: ["mid-0", "mid-1"] });
    const partial = chunks(replacement, [
      [replacement.viewportRows[0]!],
      [replacement.viewportRows[1]!],
    ]);
    held.hub.publishChunk(SESSION, partial[0]!);
    held.hub.publishFrame(SESSION, deltaFrame({ baseSeq: 1n, seq: 2n, text: "stale" }));
    held.hub.publishFrame(SESSION, deltaFrame({ baseSeq: 3n, seq: 4n, text: "live" }));
    expect(held.requests).toEqual([]);
    expect(held.hub.snapshot(SESSION)).toMatchObject({ seq: 1, valid: true });

    held.hub.publishChunk(SESSION, partial[1]!);
    expect(held.requests).toEqual([]);
    expect(texts(seededFrame(heldSink))).toEqual(["mid-0", "mid-1"]);
    expect(heldSink.deltas).toHaveLength(1);
    expect(outboundTexts(heldSink.deltas[0]!.frame)).toEqual(["live"]);
    expect(held.hub.snapshot(SESSION)).toMatchObject({ seq: 4, valid: true });

    const direct = makeHarness();
    direct.hub.expectStream(SESSION, STREAM, 8, 2);
    direct.hub.publishFrame(SESSION, fullFrame({ texts: ["old-0", "old-1"] }));
    direct.hub.publishFrame(SESSION, fullFrame({ seq: 3n, texts: ["mid-0", "mid-1"] }));
    direct.hub.publishFrame(SESSION, deltaFrame({ baseSeq: 3n, seq: 4n, text: "live" }));
    expect(held.hub.snapshot(SESSION)).toEqual(direct.hub.snapshot(SESSION));
  });

  test("falls back to the resync latch when the delta hold overflows", () => {
    const { hub, requests } = makeHarness();
    hub.expectStream(SESSION, STREAM, 8, 2);
    const source = fullFrame({ seq: 2n });
    const partial = chunks(source, [
      [source.viewportRows[0]!],
      [source.viewportRows[1]!],
    ]);
    hub.publishChunk(SESSION, partial[0]!);
    for (let held = 0; held < TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_FRAMES; held += 1) {
      hub.publishFrame(SESSION, deltaFrame());
    }
    expect(requests).toEqual([]);

    hub.publishFrame(SESSION, deltaFrame());
    expect(requests).toEqual([[SESSION, STREAM]]);
    expect(hub.snapshot(SESSION)).toBeNull();

    hub.publishChunk(SESSION, partial[1]!);
    expect(requests).toEqual([[SESSION, STREAM]]);
    expect(hub.snapshot(SESSION)).toBeNull();
  });

  test("an ordinary full mid-assembly still supersedes the partial", () => {
    const { hub, requests } = makeHarness();
    const sink = new TestSink();
    watch(hub, sink);
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame({ texts: ["old-0", "old-1"] }));
    const replacement = fullFrame({ seq: 3n, texts: ["part-0", "part-1"] });
    const partial = chunks(replacement, [
      [replacement.viewportRows[0]!],
      [replacement.viewportRows[1]!],
    ]);
    hub.publishChunk(SESSION, partial[0]!);
    hub.publishFrame(SESSION, deltaFrame({ baseSeq: 1n, seq: 2n, text: "held" }));

    hub.publishFrame(SESSION, fullFrame({ seq: 5n, texts: ["fresh-0", "fresh-1"] }));
    expect(requests).toEqual([]);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 5, valid: true });
    expect(texts(seededFrame(sink))).toEqual(["fresh-0", "fresh-1"]);

    hub.publishFrame(SESSION, deltaFrame({ baseSeq: 5n, seq: 6n, text: "after" }));
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 6, valid: true });
    expect(sink.deltas).toHaveLength(1);
    expect(outboundTexts(sink.deltas[0]!.frame)).toEqual(["after"]);
    expect(requests).toEqual([]);
  });

  test("minting a stream clears the delta hold", () => {
    const { hub, requests } = makeHarness();
    hub.expectStream(SESSION, STREAM, 8, 2);
    const stale = fullFrame({ seq: 3n });
    hub.publishChunk(SESSION, chunks(stale, [
      [stale.viewportRows[0]!],
      [stale.viewportRows[1]!],
    ])[0]!);
    for (let held = 0; held < TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_FRAMES; held += 1) {
      hub.publishFrame(SESSION, deltaFrame());
    }
    expect(requests).toEqual([]);

    hub.expectStream(SESSION, OTHER_STREAM, 8, 2);
    const fresh = fullFrame({ streamId: OTHER_STREAM, seq: 9n });
    const assembled = chunks(fresh, [
      [fresh.viewportRows[0]!],
      [fresh.viewportRows[1]!],
    ], SNAPSHOT_B);
    hub.publishChunk(SESSION, assembled[0]!);
    hub.publishFrame(SESSION, deltaFrame({ streamId: OTHER_STREAM, baseSeq: 9n, seq: 10n }));
    expect(requests).toEqual([]);

    hub.publishChunk(SESSION, assembled[1]!);
    expect(requests).toEqual([]);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 10, valid: true });
  });
});
