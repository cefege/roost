// The ack/grace gate in lib/predictiveEcho.ts: a prediction is judged only
// against grid state that could already hold its echo. Covers fast-typing
// bursts (an echo frame for an earlier keystroke must not contradict a later
// one), the unproven-write and grace windows, expiry of a never-echoed
// prediction, the re-armed confidence gate after a reset, and the backspace
// erase cell. Harness (fake DOM, clock, keystroke helpers) is shared with
// predictiveEcho.test.ts.

import { describe, test, expect } from "bun:test";
import {
  clock, frame, mkWithHost, paintedCells, type, typeUnacked,
} from "./predictiveEcho-test-harness.ts";

describe("ack-gated reconciliation", () => {
  test("an echo frame for an earlier keystroke never contradicts a later one", () => {
    // "abc" typed faster than the link echoes. The worker's write-ack and the
    // echo travel together, so when a's echo lands, b and c were acked only
    // moments ago — inside the application's own echo latency.
    const { pe } = mkWithHost("always");
    type(pe, "a");                       // write acked immediately
    typeUnacked(pe, "b");
    const seqC = typeUnacked(pe, "c");
    clock.t = 190;
    pe.noteInputWritten(seqC);           // b and c reach the PTY ~1 RTT later
    clock.t = 200;
    pe.onFrame(frame({ seq: 2, cc: 1, rows: ["a"] })); // only "a" echoed yet

    const d = pe._debug();
    expect(d.total).toBe(2);             // b and c survive as pending
    expect(d.confirmedEpoch).toBe(1);    // a's echo still unlocks the burst
  });

  test("a later coincidental match cannot unlock a tentative epoch", () => {
    const { pe } = mkWithHost("always");
    typeUnacked(pe, "a");
    const seqB = typeUnacked(pe, "b");
    pe.noteInputWritten(seqB);
    clock.t = 20;
    pe.onFrame(frame({ seq: 2, cc: 2, rows: ["zb"] }));

    expect(pe._debug()).toMatchObject({ confirmedEpoch: 0, visible: 0, total: 2 });
  });

  test("a sparse cursor frame does not double-count pending absolute columns", () => {
    const { pe } = mkWithHost("always");
    typeUnacked(pe, "abc");
    pe.onFrame(frame({ seq: 2, cc: 2, rows: [] }));

    expect(pe._debug().predCursorCol).toBe(3);
  });

  test("a prediction is not judged before its write is acknowledged", () => {
    const { pe } = mkWithHost("always");
    const seq = typeUnacked(pe, "a");
    clock.t = 200;
    // A frame the worker produced without provably having written "a" cannot
    // contradict it, however stale the prediction looks.
    pe.onFrame(frame({ seq: 2, cc: 0, rows: ["z"] }));
    expect(pe._debug().total).toBe(1);
    expect(pe._debug().confirmedEpoch).toBe(0);

    pe.noteInputWritten(seq);
    clock.t = 400;
    pe.onFrame(frame({ seq: 3, cc: 0, rows: ["z"] })); // now judgeable → wrong
    expect(pe._debug().total).toBe(0);
  });

  test("a contradiction inside the grace window is not a reset", () => {
    const { pe } = mkWithHost("always");
    type(pe, "a");
    clock.t = 20;
    pe.onFrame(frame({ seq: 2, cc: 0, rows: ["z"] })); // app may still be echoing
    expect(pe._debug().total).toBe(1);
    clock.t = 120;
    pe.onFrame(frame({ seq: 3, cc: 0, rows: ["z"] })); // outlived the grace → wrong
    expect(pe._debug().total).toBe(0);
  });

  test("a reset re-arms the confidence gate", () => {
    const { pe } = mkWithHost("always");
    type(pe, "a");
    clock.t = 200;
    pe.onFrame(frame({ seq: 2, cc: 1, rows: ["a"] }));  // confirm epoch 1
    clock.t = 210;
    type(pe, "b");
    expect(pe._debug().visible).toBe(1);                // shown on a proven epoch
    clock.t = 410;
    pe.onFrame(frame({ seq: 3, cc: 1, rows: ["ax"] })); // shown guess contradicted

    clock.t = 420;
    type(pe, "c");
    const d = pe._debug();
    // The keystroke after a reset anchors on a cursor column that still lags the
    // un-echoed input, so it must be hidden until an echo reproves the epoch.
    expect(d.visible).toBe(0);
    expect(d.predictionEpoch).toBeGreaterThan(d.confirmedEpoch);
  });

  test("expiry abandons a prediction the application never echoes", () => {
    const { pe } = mkWithHost("always");
    type(pe, "a");
    expect(pe._debug().total).toBe(1);
    clock.t = 1500;          // past PREDICTION_EXPIRE_FLOOR_MS with srtt unmeasured
    pe._expirePredictions();
    expect(pe._debug().total).toBe(0);
  });

  test("an echo that beats the write ack still unlocks the burst", () => {
    const { pe } = mkWithHost("always");
    typeUnacked(pe, "a");
    clock.t = 120;
    // The echo and the write ack race; when the echo wins, waiting for the ack
    // would hide the first chars of the burst for another whole round trip.
    pe.onFrame(frame({ seq: 2, cc: 1, rows: ["a"] }));

    const d = pe._debug();
    expect(d.total).toBe(0);
    expect(d.confirmedEpoch).toBe(1);
    expect(d.srtt).toBeGreaterThan(0);
  });

  test("a match that reproduces the cell's own text proves nothing", () => {
    const { pe } = mkWithHost("always", {
      anchor: frame({ seq: 1, full: true, cc: 0, rows: ["a"] }),
    });
    typeUnacked(pe, "a");
    clock.t = 120;
    // "a" was already at that column, so the frame is not evidence our echo
    // landed: retire the guess, but do not unlock the epoch on it.
    pe.onFrame(frame({ seq: 2, cc: 1, rows: ["a"] }));

    const d = pe._debug();
    expect(d.total).toBe(0);
    expect(d.confirmedEpoch).toBe(0);
  });
});

describe("backspace erase prediction", () => {
  test("backspace paints an erase cell, and a glyph supersedes it", () => {
    const { pe, host } = mkWithHost("always", {
      anchor: frame({ seq: 1, full: true, cc: 1, rows: ["a"] }),
    });
    type(pe, "b");
    clock.t = 200;
    pe.onFrame(frame({ seq: 2, cc: 2, rows: ["ab"] })); // confirm epoch 1

    clock.t = 210;
    type(pe, "\x7f");
    const erased = paintedCells(host);
    expect(erased).toHaveLength(1);
    expect(erased[0]!.className).toBe("cell-predict-erase");
    expect(erased[0]!.left).toBe("1ch");
    expect(erased[0]!.ch).toBe("");
    expect(pe._debug().predCursorCol).toBe(1);

    type(pe, "z");
    const retyped = paintedCells(host);
    expect(retyped).toHaveLength(1);
    expect(retyped[0]!.className).toBe("cell-predict-ch");
    expect(retyped[0]!.ch).toBe("z");
    expect(retyped[0]!.left).toBe("1ch");
  });

  test("backspace refuses a styled cell", () => {
    const { pe, host } = mkWithHost("always", {
      anchor: frame({
        seq: 1, full: true, cc: 2,
        rows: [[{ text: "ab", columns: 2, fg: 256, bg: 1, flags: 0 }]],
      }),
    });
    type(pe, "\x7f");
    expect(paintedCells(host)).toHaveLength(0);
    expect(pe._debug().total).toBe(0);
    expect(pe._debug().predictionEpoch).toBe(2); // refusal re-arms the gate
  });

  test("backspace refuses a wide glyph", () => {
    const { pe, host } = mkWithHost("always", {
      anchor: frame({
        seq: 1, full: true, cc: 2,
        rows: [[{ text: "中", columns: 2, fg: 256, bg: 256, flags: 0 }]],
      }),
    });
    type(pe, "\x7f");
    expect(paintedCells(host)).toHaveLength(0);
    expect(pe._debug().total).toBe(0);
    expect(pe._debug().predictionEpoch).toBe(2);
  });

  test("an erase match does not unlock the epoch", () => {
    const { pe } = mkWithHost("always", {
      anchor: frame({ seq: 1, full: true, cc: 2, rows: ["ab"] }),
    });
    type(pe, "\x7f");
    expect(pe._debug().total).toBe(1);
    clock.t = 200;
    pe.onFrame(frame({ seq: 2, cc: 1, rows: ["a "] })); // the cell is blank now
    const d = pe._debug();
    expect(d.total).toBe(0);          // retired
    expect(d.confirmedEpoch).toBe(0); // a blank cell is no evidence of an echo
    expect(d.srtt).toBe(0);
  });
});
