// Pins the byte reserve required before Sync v2 stamps recipient fanout timing.
// Snapshot planning must chunk before an egress stamp can exceed a cell part.
// Canonical cache rebuilds must retain ingress timing as well as cell identity.
import { expect, test } from "bun:test";
import {
  CELL_GRID_COORD_FANOUT_STAMP_MAX,
  CELL_GRID_COORD_FANOUT_STAMP_MAX_ENCODED_BYTES,
  CELL_GRID_PART_MAX_BYTES,
  encodedCellGridChunkSize,
  encodedCellGridFrameSize,
} from "@roost/shared/cell";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { terminalSnapshotSource } from "../src/connect/terminal-screen-frames.ts";
import {
  SESSION,
  STREAM,
  TestSink,
  chunks,
  deltaFrame,
  fullFrame,
  makeHarness,
  watch,
} from "./terminal-screen-hub-harness.ts";

function largestFittingFrame(
  frameForText: (textLength: number) => PbCellGridFrame,
  maximumBytes: number,
): PbCellGridFrame {
  let lower = 0;
  let upper = maximumBytes;
  let best = frameForText(0);
  while (lower <= upper) {
    const textLength = Math.floor((lower + upper) / 2);
    const candidate = frameForText(textLength);
    if (encodedCellGridFrameSize(candidate) <= maximumBytes) {
      best = candidate;
      lower = textLength + 1;
    } else {
      upper = textLength - 1;
    }
  }
  return best;
}

function expectFanoutBoundary(frame: PbCellGridFrame): void {
  const encodedBytes = encodedCellGridFrameSize(frame);
  expect(encodedBytes).toBeLessThanOrEqual(CELL_GRID_PART_MAX_BYTES);
  expect(encodedBytes).toBeGreaterThan(
    CELL_GRID_PART_MAX_BYTES - CELL_GRID_COORD_FANOUT_STAMP_MAX_ENCODED_BYTES,
  );
  expect(encodedCellGridFrameSize({
    ...frame,
    coordFanoutMs: CELL_GRID_COORD_FANOUT_STAMP_MAX,
  })).toBeGreaterThan(CELL_GRID_PART_MAX_BYTES);
}

test("plans snapshot parts with recipient fanout stamp headroom", () => {
  const source = largestFittingFrame(
    (textLength) => fullFrame({
      cols: 1,
      rows: 2,
      texts: ["x".repeat(textLength), "x".repeat(textLength)],
    }),
    CELL_GRID_PART_MAX_BYTES,
  );
  expectFanoutBoundary(source);

  const cursor = terminalSnapshotSource(() => source).createCursor();
  try {
    expect(cursor.partCount).toBeGreaterThan(1);
    for (let partIndex = 0; partIndex < cursor.partCount; partIndex++) {
      const materialized = cursor.materialize(partIndex);
      if (materialized.frame.case !== "cellGridChunk") {
        throw new Error("expected boundary snapshot to be chunked");
      }
      const chunk = materialized.frame.value;
      expect(chunk.part?.coordFanoutMs).toBe(CELL_GRID_COORD_FANOUT_STAMP_MAX);
      expect(encodedCellGridChunkSize(chunk)).toBeLessThanOrEqual(CELL_GRID_PART_MAX_BYTES);
    }
  } finally {
    cursor.release();
  }
});

test("repairs a delta that leaves no recipient fanout stamp headroom", () => {
  const delta = largestFittingFrame(
    (textLength) => deltaFrame({
      cols: 1,
      rows: 1,
      patchIndex: 0,
      text: "x".repeat(textLength),
    }),
    CELL_GRID_PART_MAX_BYTES,
  );
  expectFanoutBoundary(delta);

  const { hub, requests } = makeHarness();
  hub.expectStream(SESSION, STREAM, 1, 1);
  hub.publishFrame(SESSION, fullFrame({ cols: 1, rows: 1, texts: [""] }));
  expect(requests).toEqual([]);

  hub.publishFrame(SESSION, delta);
  expect(requests).toEqual([[SESSION, STREAM]]);
});

test("preserves ingress timing through canonical snapshot cache updates", () => {
  const { hub } = makeHarness();
  const initialSink = new TestSink();
  hub.expectStream(SESSION, STREAM, 8, 2);
  watch(hub, initialSink, "initial");
  const baseline = fullFrame();
  baseline.coordRecvMs = 711n;
  hub.publishFrame(SESSION, baseline);
  const initial = initialSink.snapshots[0]?.frames[0];
  if (!initial || initial.frame.case !== "cellGrid") {
    throw new Error("expected initial terminal snapshot");
  }
  expect(initial.frame.value.coordRecvMs).toBe(711n);

  const delta = deltaFrame();
  delta.coordRecvMs = 722n;
  hub.publishFrame(SESSION, delta);
  const recoveredSink = new TestSink();
  watch(hub, recoveredSink, "recovered");
  expect(hub.seedSocket("recovered", SESSION)).toBe(true);
  const recovered = recoveredSink.snapshots[0]?.frames[0];
  if (!recovered || recovered.frame.case !== "cellGrid") {
    throw new Error("expected recovered terminal snapshot");
  }
  expect(recovered.frame.value.coordRecvMs).toBe(722n);
});

test("uses first receipt timing across an assembled chunked snapshot", () => {
  const { hub, requests } = makeHarness();
  const sink = new TestSink();
  hub.expectStream(SESSION, STREAM, 8, 2);
  watch(hub, sink, "chunked");
  const source = fullFrame();
  const sourceChunks = chunks(source, [
    [source.viewportRows[0]!],
    [source.viewportRows[1]!],
  ]);
  hub.publishChunk(SESSION, sourceChunks[0]!, 811n);
  hub.publishChunk(SESSION, sourceChunks[1]!, 822n);
  expect(sourceChunks.map((chunk) => chunk.part?.coordRecvMs)).toEqual([811n, 811n]);
  expect(requests).toEqual([]);
  const snapshot = sink.snapshots[0]?.frames[0];
  if (!snapshot || snapshot.frame.case !== "cellGrid") {
    throw new Error("expected assembled terminal snapshot");
  }
  expect(snapshot.frame.value.coordRecvMs).toBe(811n);
});
