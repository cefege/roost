// Plan section 7 — the parser/model gate for the terminal core.
//
// Replays apps/shared/tests/fixtures/chat-tui.trace through the core the worker
// actually loads (createWtermCore -> @wterm/core 0.3.4 + the roost-patched 10k
// wasm, sha256-verified at load) at the recorded pty chunk boundaries, driving
// the real Roost emission path: nextCellFrame -> applyDelta fold -> compare
// against an all-row snapshot of the live grid, every step. A divergence FAILS
// here with the exact record (step, byte offset, chunk boundary, row/col,
// expected vs actual hash); that record is evidence, not a warning.
//
// The lanes are separate tests on purpose, and the 0.3.0 -> 0.3.4 migration is
// scored here. `fold`, `scrollback` and `boundary` are Roost's own emitter
// contract and were GREEN on both cores, which is what said the terminal-stream
// faults were never in the cell pipeline. `document`, `reply-dropped` and
// `reply-out-of-order` were RED on 0.3.0 and are now closed, each by a named
// fix: upstream's Scrollback.pop makes a vertical shrink/grow append-only, its
// bounded response FIFO replaced the single last-write-wins response slot, and
// the worker drains that FIFO with a cross-chunk carry and boundary-segmented
// writes. Every lane is GREEN, so these are regression guards now; a red lane
// here means the core artifact or the emission path moved, and the record it
// prints names which.

import { describe, test, expect } from "bun:test";
import { createWtermCore } from "../src/wterm-core-factory.ts";
import { parseTrace, replayTrace, reportOn, type TraceReport } from "./trace-oracle.ts";

const FIXTURE = new URL("./fixtures/chat-tui.trace", import.meta.url);

const program = parseTrace(await Bun.file(FIXTURE).text());
const report: TraceReport = await replayTrace(program, { createCore: createWtermCore });

describe("pinned-core trace oracle", () => {
  test("the trace exercises every surface the core upgrade would move", () => {
    // Guard the oracle itself: a fixture that quietly stopped covering a surface
    // would turn every assertion below it into a green no-op.
    const c = report.coverage;
    expect(program.steps.length).toBe(666);
    expect(program.ordinals).toBe(10_824);
    expect(c.writeChunks).toBe(642);
    expect(c.bytes).toBe(605_417);
    expect([...c.dimensions].sort())
      .toEqual(["100x24", "60x24", "80x12", "80x24", "80x30", "90x24"]);
    expect(c.resizes).toBe(11);
    // Fourteen epochs, one per semantic reframe: eleven resizes plus both
    // alt-screen transitions. A forced attach snapshot must NOT advance the
    // epoch, which is what keeps epochAdvances one below the epoch count.
    expect(c.epochs.length).toBe(14);
    expect(c.epochAdvances).toBe(13);
    expect(c.framesFull).toBe(17);
    expect(c.framesDelta).toBe(639);
    expect(c.deltaRowsMax).toBe(30);
    expect(c.distinctGridHashes).toBeGreaterThan(600);
    expect(c.nativeQueries).toBe(5);
    expect(c.synthQueries).toBe(4);
    expect(c.splitQueries).toBe(1);
    expect(c.syncBoundaries).toBe(8);
    expect(c.altTransitions).toBe(2);
    expect(c.wideCells).toBeGreaterThan(200);
    expect(c.astralCells).toBeGreaterThan(20);
    expect(c.graphemeCells).toBeGreaterThan(20);
    expect(c.cursorHiddenSteps).toBeGreaterThan(0);
    expect(c.cursorKeysAppSteps).toBeGreaterThan(0);
    expect(c.bracketedPasteSteps).toBeGreaterThan(0);
    expect(c.attachFullFrames).toBe(3);
    expect(c.sweeps).toBe(7);
    // The ring must genuinely roll over, not merely fill: a trace stopping one
    // line short never exercises the eviction origin at all.
    expect(c.scrollbackMax).toBe(10_000);
    // The eviction origin at every named checkpoint, which is where the
    // authoritative-counter cutover is pinned. `after-rollover-tail` is the
    // trace's original end point: 794 is exactly what Roost's retired
    // tail-signature INFERENCE produced there, and it is what
    // getScrollbackDiscardedCount() reports now — measured equal at all 643 emit
    // steps of that prefix, not merely at its end.
    //
    // 794 where 0.3.0 recorded 800: the `resize 80 30` at fixture line 88 pops
    // 14 rows back OUT of history into the grown viewport (0.3.0 had no
    // Scrollback.pop and stranded them there, which is the same fault the
    // `document` lane caught), so history is shorter for the rest of the run and
    // the 10k ring ends six evictions short of the old total.
    //
    // The two checkpoints after it are the fixture's extension: a vertical
    // shrink AND grow while the ring is at capacity, then a synchronized-output
    // frame spanning a resize. The saturated grow pops rows out of a ring that
    // IS discarding, and the origin holds — it does not rewind, which is what
    // would silently re-alias every absolute index the browser holds.
    expect(c.discardedAtSweep).toEqual({
      "after-horizontal-resize": 0,
      "after-vertical-resize": 0,
      "after-rollover": 792,
      "after-rollover-tail": 794,
      "after-saturated-resize": 808,
      "after-synchronized-resize": 808,
      "final": 808,
    });
    expect(c.scrollbackDiscarded).toBe(808);
    // Steps at which the ring was actually DISCARDING. The retired band-based
    // probe armed for 300 steps merely NEAR capacity; only these 52 exercise the
    // origin at all. The `scrollback` lane re-derives it from the trace's own
    // line ordinals at every one of them.
    expect(c.ringEvictingSteps).toBe(52);
    // Ran against the patched 10k wasm. There is no 1k stock fallback left to
    // fall back to: an unverifiable module fails worker readiness instead.
    expect(report.observations.scrollbackCapacity).toBe(10_000);
  });

  test("folding the emitted frames equals an all-row snapshot at every step", () => {
    expect(reportOn(report, "fold")).toBe("fold: clean");
    expect(report.abortedAtStep).toBeNull();
  });

  test("the folded history tracks the ring, origin included, across rollover", () => {
    expect(reportOn(report, "scrollback")).toBe("scrollback: clean");
  });

  test("re-cutting the same bytes at different boundaries paints the same grid", () => {
    expect(reportOn(report, "boundary")).toBe("boundary: clean");
  });

  test("a refresh recovers only future output; history needs the explicit backfill", () => {
    // Authoritative full frames are viewport-only (SB_SNAPSHOT_HISTORY_ROWS = 0),
    // so a reattach restarts the browser's held history at the current total and
    // accumulated output returns solely through readScrollbackRangeCells — and
    // only for lines the ring still retains. Plan section 7 asks the trace to
    // record exactly this, so it is pinned rather than merely reported.
    const refreshes = report.observations.refreshes;
    expect(refreshes.length).toBe(3);
    for (const r of refreshes) {
      expect(r.recovers).toBe("future-only");
      expect(r.fullFrameHistoryRows).toBe(0);
      expect(r.backfillRows).toBe(Math.min(r.scrollbackTotal, 10_000));
    }
    // The post-rollover reattach proves the lossy half: 10,792 lines were
    // written into the ring (0.3.0: 10,798 — see the eviction pin above),
    // 10,000 are still addressable, 792 are gone for good.
    expect(refreshes[2]!.scrollbackTotal).toBe(10_792);
    expect(refreshes[2]!.backfillRows).toBe(10_000);
  });

  test("a wide codepoint owns a lead cell plus a continuation cell", () => {
    // 0.3.4 models display width: a width-2 lead is followed by a width-0
    // continuation whose char is 0, so the core's column arithmetic (wrap,
    // cursor, clipping) finally agrees with what a terminal paints. 0.3.0 was
    // "lead-only" and disagreed. This pins the model the cell contract's
    // explicit span columns are built on; a core that regressed to lead-only
    // would silently re-open the grapheme-slicing class.
    expect(report.observations.wideCellModel).toBe("lead-plus-continuation");
  });

  test("a vertical shrink keeps the document append-only", () => {
    // Was RED on 0.3.0: `resize 80 12` appended the rows the user was looking
    // at — live tail, cursor line — to the MIDDLE of scrollback and kept the
    // oldest rows on screen, so history++viewport stopped being the document
    // the stream wrote. 0.3.4's Scrollback.push/pop ordering closes it; this
    // assertion is the regression guard for the fix, not a pending item.
    expect(reportOn(report, "document")).toBe("document: clean");
  });

  test("every capability probe is answered", () => {
    // Was RED on 0.3.0 for two independent reasons: a probe split across pty
    // chunks was never answered (the synthetic scan was per-chunk with no
    // carry), and two probes in one chunk yielded one reply because the core
    // kept a single response slot the last write clobbered. 0.3.4 replaced the
    // slot with a bounded FIFO and the worker now drains it until empty, so
    // every queued reply reaches the pty.
    expect(reportOn(report, "reply-dropped")).toBe("reply-dropped: clean");
    expect(report.observations.queryReplyModel).toBe("ordered-drain");
  });

  test("replies reach the pty in the order their probes appeared", () => {
    // Was RED on 0.3.0: the worker concatenated the core's native reply ahead
    // of every synthesized one, so `ESC[c` then `ESC[6n` in one chunk answered
    // CPR first. Segmented writes at synthetic-query boundaries close it.
    expect(reportOn(report, "reply-out-of-order")).toBe("reply-out-of-order: clean");
  });
});
