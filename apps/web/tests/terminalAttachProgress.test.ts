// Attach-progress exposure on the view handle: subscribeProgress must report
// the replica assembler's chunked-baseline progress while a baseline is
// assembling and fall back to null (indeterminate) on completion and reset —
// the exact states the loading card's meter renders. Driven through
// dispatchTerminalCellChunk like terminalStream.test.ts.
//
// The multi-chunk fixture (256×256 grid with hyperlink payloads, same shape
// as terminalStream.test.ts's chunked baseline) crosses the one-MiB part
// bound several times, so the assembler really holds a partial. It is built
// once and reused across tests; protobuf encoding that much wire costs
// seconds, hence the generous per-test timeouts.

import { describe, expect, test, vi } from "bun:test";
import {
  SESSION_ID,
  SNAPSHOT_A,
  STREAM_A,
  acceptView,
  cellFrameToProto,
  chunkCellGridFrame,
  full,
  latestViewCommand,
  terminalStream,
} from "./helpers/terminalStreamFixture.ts";
import type { BaselineProgress } from "../src/store/terminal-stream-types.ts";
import type { PbCellGridChunk } from "@roost/shared/proto/cell_pb";

let cachedChunks: PbCellGridChunk[] | null = null;

function multiChunkBaseline(): PbCellGridChunk[] {
  if (cachedChunks) return cachedChunks;
  const largeRows = Array.from({ length: 256 }, (_, rowIndex) => ({
    index: rowIndex,
    spans: Array.from({ length: 256 }, (_, colIndex) => {
      const mapping = (rowIndex * 256 + colIndex) % 1_024;
      return {
        text: "x",
        columns: 1,
        fg: 256,
        bg: 256,
        flags: 0,
        linkKey: `link-${mapping}`,
        linkUri: `https://example.invalid/${mapping}/${"u".repeat(64)}`,
      };
    }),
  }));
  const pb = cellFrameToProto(full(STREAM_A, largeRows), SESSION_ID);
  const chunks = chunkCellGridFrame(pb, SNAPSHOT_A);
  if (chunks.length < 3) throw new Error("fixture produced too few chunks");
  cachedChunks = chunks;
  return chunks;
}

function acceptedView(): ReturnType<typeof terminalStream.createTerminalView> {
  const view = terminalStream.createTerminalView(SESSION_ID);
  view.setViewport({ cols: 256, rows: 256 });
  acceptView(view.viewId, latestViewCommand().value.revision as bigint, STREAM_A, 256, 256);
  return view;
}

describe("view handle subscribeProgress", () => {
  test("emits assembly progress per poll and null on completion", () => {
    const view = acceptedView();
    const chunks = multiChunkBaseline();
    const emissions: Array<BaselineProgress | null> = [];
    const release = view.subscribeProgress((progress) => emissions.push(progress));
    // Immediate current value: idle replica → one null emission up front.
    expect(emissions).toEqual([null]);

    vi.advanceTimersByTime(200);
    expect(emissions).toEqual([null]);
    terminalStream.dispatchTerminalCellChunk(chunks[0]!);
    vi.advanceTimersByTime(200);
    expect(emissions.at(-1)).toEqual({
      snapshotId: SNAPSHOT_A, receivedChunks: 1, totalChunks: chunks.length,
    });

    for (const chunk of chunks.slice(1)) {
      terminalStream.dispatchTerminalCellChunk(chunk);
      vi.advanceTimersByTime(200);
    }
    // Completed assembly is idle again: the meter returns to indeterminate.
    expect(emissions.filter((progress) => progress === null).length)
      .toBeGreaterThanOrEqual(2);
    release();
    vi.advanceTimersByTime(200 * 4);
    // Unsubscribing stops the poller: no further emissions were appended.
    expect(emissions.length).toBeLessThan(30);
    view.dispose();
  }, 30_000);

  test("a mid-assembly invalid chunk resets progress to null", () => {
    const view = acceptedView();
    const chunks = multiChunkBaseline();
    const emissions: Array<BaselineProgress | null> = [];
    view.subscribeProgress((progress) => emissions.push(progress));

    terminalStream.dispatchTerminalCellChunk(chunks[0]!);
    vi.advanceTimersByTime(200);
    expect(emissions.at(-1)?.receivedChunks).toBe(1);

    // Out-of-order chunk trips the resync path, which drops the partial.
    terminalStream.dispatchTerminalCellChunk(chunks[2]!);
    vi.advanceTimersByTime(200);
    expect(emissions.at(-1)).toBeNull();
    view.dispose();
  }, 30_000);

  test("dispose clears the poller with listeners attached", () => {
    const view = acceptedView();
    multiChunkBaseline();
    let calls = 0;
    view.subscribeProgress(() => { calls += 1; });
    vi.advanceTimersByTime(200);
    const before = calls;
    expect(before).toBeGreaterThan(0);
    view.dispose();
    vi.advanceTimersByTime(200 * 5);
    expect(calls).toBe(before);
  }, 30_000);
});
