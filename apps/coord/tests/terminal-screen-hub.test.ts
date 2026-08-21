import { describe, expect, test } from "bun:test";
import {
  EPOCH,
  OTHER_STREAM,
  SESSION,
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
describe("TerminalScreenHub canonical cache", () => {
  test("folds deltas once and falls back to the folded baseline when a socket cursor rejects", () => {
    const { hub } = makeHarness();
    const incremental = new TestSink(true);
    const needsBaseline = new TestSink(false);
    watch(hub, incremental, "incremental");
    watch(hub, needsBaseline, "needs-baseline");
    hub.expectStream(SESSION, STREAM, 8, 2);

    hub.publishFrame(SESSION, fullFrame({ texts: ["old-a", "old-b"] }));
    expect(incremental.snapshots).toHaveLength(1);
    expect(needsBaseline.snapshots).toHaveLength(1);

    hub.publishFrame(SESSION, deltaFrame({ text: "new-b" }));
    expect(incremental.deltas).toHaveLength(1);
    expect(incremental.snapshots).toHaveLength(1);
    expect(needsBaseline.deltas).toHaveLength(1);
    expect(needsBaseline.snapshots).toHaveLength(2);

    const folded = seededFrame(needsBaseline);
    expect(folded).toMatchObject({
      sessionId: SESSION,
      streamId: STREAM,
      gridEpoch: EPOCH,
      full: true,
      seq: 2n,
      baseSeq: 0n,
      cursorRow: 1,
      cursorCol: 2,
      cursorVisible: false,
      cursorKeysApp: true,
      bracketedPaste: true,
      mouseTracking: 1000,
      mouseSgr: true,
      focusEvents: true,
    });
    expect(texts(folded)).toEqual(["old-a", "new-b"]);
    expect(hub.snapshot(SESSION)).toMatchObject({
      streamId: STREAM,
      gridEpoch: EPOCH,
      seq: 2,
      cols: 8,
      rows: 2,
      valid: true,
    });

    const late = new TestSink();
    watch(hub, late, "late");
    expect(late.begins).toEqual([[SESSION, STREAM]]);
    expect(late.snapshots).toHaveLength(0);
    hub.seedSocket("late", SESSION);
    expect(texts(seededFrame(late))).toEqual(["old-a", "new-b"]);
  });

  test("publishes activation before cells and drops hidden socket state", () => {
    const { hub } = makeHarness();
    const sink = new TestSink();
    hub.registerSocket("socket-a", sink);
    hub.setWatching("socket-a", SESSION, true);
    expect(sink.begins).toHaveLength(0);

    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame());
    expect(sink.events).toEqual([
      `begin:${STREAM}`,
      `snapshot:${STREAM}`,
    ]);

    hub.setWatching("socket-a", SESSION, false);
    expect(sink.drops).toEqual([SESSION]);
    hub.expectStream(SESSION, OTHER_STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame({ streamId: OTHER_STREAM, seq: 3n }));
    expect(sink.begins).toHaveLength(1);
    expect(sink.snapshots).toHaveLength(1);

    hub.setWatching("socket-a", SESSION, true);
    expect(sink.begins.at(-1)).toEqual([SESSION, OTHER_STREAM]);
    hub.seedSocket("socket-a", SESSION);
    expect(seededFrame(sink).streamId).toBe(OTHER_STREAM);
  });

  test("ignores stale streams and latches one repair across wrong base and epoch", () => {
    const { hub, requests } = makeHarness();
    const sink = new TestSink();
    watch(hub, sink);
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame());

    hub.publishFrame(SESSION, deltaFrame({ streamId: OTHER_STREAM }));
    expect(requests).toHaveLength(0);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 1, valid: true });

    hub.publishFrame(SESSION, deltaFrame({ baseSeq: 9n, seq: 10n }));
    hub.publishFrame(SESSION, deltaFrame({ epoch: "wrong-epoch" }));
    expect(requests).toEqual([[SESSION, STREAM]]);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 1, valid: false });

    hub.publishFrame(SESSION, fullFrame({ seq: 10n }));
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 10, valid: true });
    hub.publishFrame(SESSION, deltaFrame({ baseSeq: 10n, seq: 11n, epoch: "wrong-again" }));
    expect(requests).toEqual([
      [SESSION, STREAM],
      [SESSION, STREAM],
    ]);
  });

  test("keeps the old baseline visible until a complete replacement assembles", () => {
    const { hub, requests } = makeHarness();
    const sink = new TestSink();
    watch(hub, sink);
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame({ seq: 1n, texts: ["old-0", "old-1"] }));

    const replacement = fullFrame({ seq: 2n, texts: ["new-0", "new-1"] });
    const firstAttempt = chunks(replacement, [
      [replacement.viewportRows[0]!],
      [replacement.viewportRows[1]!],
    ]);
    hub.publishChunk(SESSION, firstAttempt[0]!);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 1, valid: true });
    expect(sink.snapshots).toHaveLength(1);

    hub.publishChunk(SESSION, firstAttempt[0]!);
    expect(requests).toEqual([[SESSION, STREAM]]);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 1, valid: true });
    expect(texts(seededFrame(sink))).toEqual(["old-0", "old-1"]);

    const complete = chunks(replacement, [
      [replacement.viewportRows[0]!],
      [replacement.viewportRows[1]!],
    ], SNAPSHOT_B);
    hub.publishChunk(SESSION, complete[0]!);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 1, valid: true });
    hub.publishChunk(SESSION, complete[1]!);
    expect(hub.snapshot(SESSION)).toMatchObject({ seq: 2, valid: true });
    expect(sink.snapshots).toHaveLength(2);
    expect(texts(seededFrame(sink))).toEqual(["new-0", "new-1"]);
  });
});
