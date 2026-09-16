// Covers lazy terminal snapshot source construction, immutable predecessor
// cursors, and the first-byte deadline a freshly expected stream arms.
// The canonical-cache suite owns folding and watcher behavior; this suite owns
// cursor demand and the "worker committed a stream and shipped no baseline" net.

import { describe, expect, test } from "bun:test";
import type { FirehoseFrame } from "@roost/shared/proto/sync_pb";
import type {
  TerminalSnapshotCursor,
  TerminalSnapshotSource,
} from "../src/connect/terminal-screen-frames.ts";
import type {
  TerminalDeltaEnqueueResult,
  TerminalScreenSocketSink,
} from "../src/connect/terminal-screen-hub.ts";
import {
  SESSION,
  OTHER_STREAM,
  STREAM,
  chunks,
  deltaFrame,
  fullFrame,
  makeHarness,
  texts,
} from "./terminal-screen-hub-harness.ts";
import { TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS } from "../src/connect/terminal-screen-hub.ts";

class DeferredSnapshotSink implements TerminalScreenSocketSink {
  readonly begins: Array<[sessionId: string, streamId: string]> = [];
  readonly states: Array<{ frame: FirehoseFrame; sessionId: string }> = [];
  readonly deltas: Array<{
    sessionId: string;
    streamId: string;
    frame: FirehoseFrame;
  }> = [];
  readonly drops: string[] = [];
  readonly sources: Array<{
    sessionId: string;
    streamId: string;
    source: TerminalSnapshotSource;
  }> = [];

  beginTerminalStream(sessionId: string, streamId: string): boolean {
    this.begins.push([sessionId, streamId]);
    return true;
  }

  enqueueTerminalState(frame: FirehoseFrame, sessionId: string): void {
    this.states.push({ frame, sessionId });
  }

  replaceTerminalSnapshot(
    sessionId: string,
    streamId: string,
    source: TerminalSnapshotSource,
  ): boolean {
    this.sources.push({ sessionId, streamId, source });
    return true;
  }

  enqueueTerminalDelta(
    sessionId: string,
    streamId: string,
    frame: FirehoseFrame,
  ): TerminalDeltaEnqueueResult {
    this.deltas.push({ sessionId, streamId, frame });
    return "queued";
  }

  dropTerminalSession(sessionId: string): void {
    this.drops.push(sessionId);
  }
}

function frameFromSnapshotCursor(cursor: TerminalSnapshotCursor) {
  expect(cursor.partCount).toBe(1);
  const frame = cursor.materialize(0);
  if (frame.frame.case !== "cellGrid") throw new Error("expected unchunked terminal snapshot");
  return frame.frame.value;
}

describe("TerminalScreenHub snapshot sources", () => {
  test("defers accepted full and delta snapshots until cursor demand", () => {
    const { hub } = makeHarness();
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame({ texts: ["first", "second"] }));
    hub.publishFrame(SESSION, deltaFrame({ text: "updated" }));

    const sink = new DeferredSnapshotSink();
    hub.registerSocket("demand", sink);
    hub.setWatching("demand", SESSION, true);
    expect(sink.sources).toHaveLength(0);

    expect(hub.seedSocket("demand", SESSION)).toBe(true);
    expect(sink.sources).toHaveLength(1);
    const firstSource = sink.sources[0];
    if (!firstSource) throw new Error("expected first snapshot source");

    expect(hub.seedSocket("demand", SESSION)).toBe(true);
    expect(sink.sources).toHaveLength(2);
    const repeatedSource = sink.sources[1];
    if (!repeatedSource) throw new Error("expected memoized snapshot source");
    expect(repeatedSource.source).toBe(firstSource.source);

    const cursor = firstSource.source.createCursor();
    try {
      const encoded = frameFromSnapshotCursor(cursor);
      expect(encoded.sessionId).toBe(SESSION);
      expect(encoded.seq).toBe(2n);
      expect(texts(encoded)).toEqual(["first", "updated"]);
    } finally {
      cursor.release();
    }
  });

  test("keeps a leased predecessor cursor immutable after the canonical cache advances", () => {
    const { hub } = makeHarness();
    const predecessor = new DeferredSnapshotSink();
    hub.registerSocket("predecessor", predecessor);
    hub.setWatching("predecessor", SESSION, true);
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame({ texts: ["old-a", "old-b"] }));

    const predecessorSource = predecessor.sources[0];
    if (!predecessorSource) throw new Error("expected predecessor snapshot source");
    const predecessorCursor = predecessorSource.source.createCursor();
    try {
      hub.publishFrame(SESSION, deltaFrame({ text: "new-b" }));
      expect(hub.snapshot(SESSION)).toMatchObject({ seq: 2, valid: true });

      const successor = new DeferredSnapshotSink();
      hub.registerSocket("successor", successor);
      hub.setWatching("successor", SESSION, true);
      expect(successor.sources).toHaveLength(0);
      expect(hub.seedSocket("successor", SESSION)).toBe(true);

      const successorSource = successor.sources[0];
      if (!successorSource) throw new Error("expected successor snapshot source");
      expect(successorSource.source).not.toBe(predecessorSource.source);
      const successorCursor = successorSource.source.createCursor();
      try {
        const predecessorFrame = frameFromSnapshotCursor(predecessorCursor);
        expect(predecessorFrame.seq).toBe(1n);
        expect(texts(predecessorFrame)).toEqual(["old-a", "old-b"]);

        const successorFrame = frameFromSnapshotCursor(successorCursor);
        expect(successorFrame.seq).toBe(2n);
        expect(texts(successorFrame)).toEqual(["old-a", "new-b"]);
      } finally {
        successorCursor.release();
      }
    } finally {
      predecessorCursor.release();
    }
  });
});

describe("TerminalScreenHub baseline watchdog", () => {
  test("escalates a minted stream whose baseline never arrives", () => {
    const clock = { value: 0 };
    const { hub, requests, freshStreams, timers, fireTimer } = makeHarness(clock);
    hub.expectStream(SESSION, STREAM, 8, 2);
    expect(requests).toEqual([]);
    expect(timers.size).toBe(1);
    const [watchdog, deadline] = [...timers.entries()][0]!;
    expect(deadline.delayMs).toBe(TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS);

    clock.value += TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS;
    fireTimer(watchdog);
    expect(requests).toEqual([[SESSION, STREAM]]);

    const firstAttempt = [...timers.keys()][0]!;
    clock.value += TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS;
    fireTimer(firstAttempt);
    expect(requests).toHaveLength(2);
    expect(freshStreams).toEqual([]);

    const secondAttempt = [...timers.keys()][0]!;
    clock.value += TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS;
    fireTimer(secondAttempt);
    expect(requests).toHaveLength(2);
    expect(freshStreams).toEqual([[
      SESSION,
      STREAM,
      expect.stringContaining("timed out"),
    ]]);
    expect(timers.size).toBe(0);
  });

  test("asks for nothing when the baseline lands before the deadline", () => {
    const clock = { value: 0 };
    const { hub, requests, freshStreams, timers } = makeHarness(clock);
    hub.expectStream(SESSION, STREAM, 8, 2);
    expect(timers.size).toBe(1);

    hub.publishFrame(SESSION, fullFrame());
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 1, valid: true });
    expect(timers.size).toBe(0);

    clock.value += TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS * 4;
    hub.publishFrame(SESSION, deltaFrame({ text: "live" }));
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 2, valid: true });
    expect(requests).toEqual([]);
    expect(freshStreams).toEqual([]);
  });

  test("a re-minted stream deadline replaces the superseded one", () => {
    const clock = { value: 0 };
    const { hub, requests, timers, fireTimer } = makeHarness(clock);
    hub.expectStream(SESSION, STREAM, 8, 2);
    const [stale, superseded] = [...timers.entries()][0]!;

    hub.expectStream(SESSION, OTHER_STREAM, 8, 2);
    expect(timers.has(stale)).toBe(false);

    clock.value += TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS;
    superseded.callback();
    expect(requests).toEqual([]);

    const fresh = [...timers.keys()][0]!;
    expect(fresh).not.toBe(stale);
    fireTimer(fresh);
    expect(requests).toEqual([[SESSION, OTHER_STREAM]]);
  });

  test("leaves a chunked baseline mid-transfer to the chunk stall deadline", () => {
    const clock = { value: 0 };
    const { hub, requests, timers } = makeHarness(clock);
    hub.expectStream(SESSION, STREAM, 8, 2);
    const [watchdog, deadline] = [...timers.entries()][0]!;

    const source = fullFrame();
    const parts = chunks(source, [
      [source.viewportRows[0]!],
      [source.viewportRows[1]!],
    ]);
    clock.value += Math.floor(TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS / 2);
    hub.publishChunk(SESSION, parts[0]!);

    clock.value += TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS
      - Math.floor(TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS / 2);
    deadline.callback();
    expect(requests).toEqual([]);
    expect(timers.size).toBe(1);
    expect([...timers.keys()][0]!).not.toBe(watchdog);

    hub.publishChunk(SESSION, parts[1]!);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 1, valid: true });
    expect(requests).toEqual([]);
  });
});
