// CellGridRenderer DOM tripwire — the held scrollback window.
//
// Eviction caps the rows this document keeps, and every evicted SB_BLOCK must
// leave a placeholder whose reserved height is EXACT — otherwise the scroll
// space lies and the corruption class returns as a jump instead of a rewrite.
// Companion to cellRenderer.append.dom.test.ts, which owns the growth side.

import { describe, test, expect } from "bun:test";
import {
  CellGridRenderer,
  blockPlaceholder,
  MAX_HELD_SCROLLBACK_ROWS,
} from "../src/lib/cellRenderer.ts";
import { spansText, type CellGridFrame, type CellRow } from "@roost/shared/cell";
import {
  PAD_TOP,
  ROW_PX,
  FakeEl,
  makeContainer,
  row,
  deltaFrame,
  seedHeldHistory,
  sbEl,
  sbRows,
} from "./helpers/cellRendererFakeDom.ts";

// ── client-side eviction: cap the held scrollback window ─────────────────
// .cell-scrollback was append-only, so a long stable streaming session grew
// live DOM nodes ~500/min without bound (the long-uptime lag). _evictScrollback
// trims oldest whole content-visibility blocks once the held window exceeds
// MAX_HELD_SCROLLBACK_ROWS, bumping sbBase so the held-window invariant
// (scrollbackRows.length === scrollbackTotal - sbBase) stays honest and
// scrollbackBackfill re-pulls the evicted range on scroll-up. These lock the
// cap, the invariant + DOM↔array alignment, and the freeze under a scrolled-up
// reader.
// leading block never desyncs (every backfill prepend is < SB_BLOCK because
// the overlap row is stripped at scrollbackBackfill.ts:111).
describe("CellGridRenderer DOM — held-window eviction", () => {
  const BLOCK = 250; // mirrors cellRenderer SB_BLOCK
  // Delta that appends `append` scrollback rows, carrying the cumulative
  // absolute `total` (applyDelta takes scrollbackTotal verbatim from the delta).
  const appDelta = (append: CellRow[], total: number, seq: number): CellGridFrame =>
    ({ ...deltaFrame(80, 1, [row(0, "v")], append, seq), scrollbackTotal: total });
  const seq = (n: number) => Array.from({ length: n }, (_, i) => i);
  const grow = (r: CellGridRenderer, from: number, batches: number) => {
    let total = from, idx = from;
    for (let i = 0; i < batches; i++) {
      const append = seq(BLOCK).map((k) => row(idx + k, `s${idx + k}`));
      idx += BLOCK; total += BLOCK;
      r.apply(appDelta(append, total, i + 3));
    }
    return { total, idx };
  };

  test("eviction caps the held window and preserves invariant + DOM alignment", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = sbEl(c);
    seedHeldHistory(r, 80, [row(0, "v")], seq(100).map((i) => row(i, `h${i}`)));
    const { total, idx } = grow(r, 100, 12);
    // After every apply the invariant, the cap, and DOM↔array alignment hold.
    const f = r.currentFrame!;
    expect(f.scrollbackTotal - f.sbBase).toBe(f.scrollbackRows.length);
    expect(f.scrollbackRows.length).toBeLessThanOrEqual(MAX_HELD_SCROLLBACK_ROWS);
    expect(sbRows(scrollbackEl).length).toBe(f.scrollbackRows.length);
    // Block count bounded (≤ ceil(MAX_HELD / BLOCK) + 1 open tail block).
    expect(scrollbackEl.children.length).toBeLessThanOrEqual(Math.ceil(MAX_HELD_SCROLLBACK_ROWS / BLOCK) + 1);
    // Tail row survived eviction unchanged.
    expect(spansText((f.scrollbackRows[f.scrollbackRows.length - 1]!).spans)).toBe(`s${idx - 1}`);
    expect(total).toBe(idx); // sanity: total tracks the last appended index
  });

  test("returning to the bottom reconciles pending history and re-enables eviction", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], seq(100).map((i) => row(i, `h${i}`)));
    c.scrollTop = c.scrollHeight - c.clientHeight - ROW_PX;
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    c.resetScrollTopWrites();
    grow(r, 100, 8);
    expect(r.readerIntent).toBe("reading");
    expect(r.currentFrame!.scrollbackRows.length).toBe(100);
    expect(r.canonicalFrameSeq()).toBe(10);
    expect(c.scrollTopWrites).toBe(0);

    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();
    expect(r.handleScroll()).toEqual({ reconciled: true, anchorChanged: true });
    const f = r.currentFrame!;
    expect(f.scrollbackRows.length).toBeLessThanOrEqual(MAX_HELD_SCROLLBACK_ROWS);
    expect(f.scrollbackTotal - f.sbBase).toBe(f.scrollbackRows.length);
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
  });

  test("a partial leading block (backfill prepend) never desyncs DOM from array", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = sbEl(c);
    // Tail frame (sbBase > 0): held = last 100 rows of a 2500-row history.
    const total = 2500;
    const tailStart = total - 100;
    seedHeldHistory(r, 80, [row(0, "v")], seq(100).map((k) => row(tailStart + k, `h${tailStart + k}`)), total);
    // Backfill with a PARTIAL chunk (< BLOCK): every real backfill batch is
    // < SB_BLOCK (overlap row stripped), so this is the realistic case. The
    // leading block becomes partial (180 rows) — the first block eviction removes.
    const chunk = 180;
    r.prependScrollback(seq(chunk).map((k) => row(tailStart - chunk + k, `b${k}`)));
    expect(r.currentFrame!.scrollbackRows.length).toBe(100 + chunk);
    // Stream past the cap; check invariant + DOM alignment every apply.
    let idx = total, running = total;
    for (let i = 0; i < 12; i++) {
      const append = seq(BLOCK).map((k) => row(idx + k, `s${idx + k}`));
      idx += BLOCK; running += BLOCK;
      r.apply(appDelta(append, running, i + 3));
      const f = r.currentFrame!;
      expect(f.scrollbackTotal - f.sbBase).toBe(f.scrollbackRows.length);
      expect(f.scrollbackRows.length).toBeLessThanOrEqual(MAX_HELD_SCROLLBACK_ROWS);
      // Killer assertion: painted DOM row count must track the array. A hardcoded
      // dropped = SB_BLOCK would leave the 180-row block's worth of DOM behind
      // while slicing 250 off the array → DOM count > array length.
      expect(sbRows(scrollbackEl).length).toBe(f.scrollbackRows.length);
    }
  });

  test("only the mutable tail is excluded from browser anchoring", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = sbEl(c);
    seedHeldHistory(r, 80, [row(0, "v")], seq(300).map((i) => row(300 + i, `h${i}`)), 600);
    expect((scrollbackEl.children[0] as FakeEl).style["overflow-anchor"]).toBeUndefined();
    expect((scrollbackEl.children[1] as FakeEl).style["overflow-anchor"]).toBe("none");

    r.prependScrollback([row(299, "backfill")]);
    expect((scrollbackEl.children[2] as FakeEl).style["overflow-anchor"]).toBeUndefined();

    r.apply(appDelta([row(600, "stream")], 601, 3));
    expect((scrollbackEl.children[2] as FakeEl).style["overflow-anchor"]).toBe("none");
  });

  test("explicit reading leaves the inspected viewport and painted history untouched", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    c.clientHeight = 500;
    seedHeldHistory(r, 80, [row(0, "v")], seq(2000).map((i) => row(i, `h${i}`)));
    c.scrollTop = PAD_TOP + 800 * ROW_PX;
    r.handleScroll();
    const held = c.scrollTop;
    const baseBefore = r.currentFrame!.sbBase;
    const append = seq(BLOCK).map((k) => row(2000 + k, `s${2000 + k}`));
    c.resetScrollTopWrites();
    r.apply(appDelta(append, 2000 + BLOCK, 3));
    expect(r.currentFrame!.sbBase).toBe(baseBefore);
    expect(r.currentFrame!.scrollbackRows.length).toBe(2000);
    expect(r.canonicalFrameSeq()).toBe(3);
    expect(c.scrollTop).toBe(held);
    expect(c.scrollTopWrites).toBe(0);
  });
});

describe("CellGridRenderer DOM — content-visibility placeholder exactness", () => {
  const BLOCK = 250; // mirrors cellRenderer SB_BLOCK
  const seqN = (n: number) => Array.from({ length: n }, (_, i) => i);
  // FakeStyle records setProperty() calls as own keys; read one back by name.
  const csz = (el: FakeEl): string | undefined =>
    (el.style as unknown as Record<string, string>)["contain-intrinsic-size"];

  test("a block's skipped-state placeholder is its EXACT measured height, partial or full", () => {
    // A skipped content-visibility block reports contain-intrinsic-size, not its
    // content. A flat estimate overstates every partial block, so the block
    // reflows when it materializes and every row below it shifts — the "scroll
    // jumps around" class. Placeholder must equal truth for BOTH shapes, in the
    // MEASURED row height (ROW_PX here) rather than a hardcoded em.
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const sb: FakeEl = sbEl(c);
    seedHeldHistory(r, 80, [row(0, "v")], seqN(7).map((i) => row(i, `s${i}`)));
    expect(sb.children.length).toBe(1);
    expect(csz(sb.children[0])).toBe("112.00px");   // 7 rows × 16px

    // Cross a block boundary: the closed block is exactly SB_BLOCK rows, the new
    // open block carries the remainder.
    const append = seqN(BLOCK).map((k) => row(7 + k, `s${7 + k}`));
    r.apply({ ...deltaFrame(80, 1, [row(0, "v")], append, 3), scrollbackTotal: 7 + BLOCK });
    expect(sb.children.length).toBe(2);
    expect(csz(sb.children[0])).toBe("4000.00px"); // 250 × 16px, the full block
    expect(csz(sb.children[1])).toBe("112.00px");  // 257 - 250 = 7 rows
  });

  test("only the OPEN tail block opts out of content-visibility; sealing restores it", () => {
    // A skipped subtree contributes its last-EVALUATED intrinsic size, and that is
    // re-evaluated at rendering-lifecycle time, not when rows are appended. So
    // appending into a skipped tail leaves scrollHeight stale for the rest of the
    // task, and apply()'s pre-mutation atBottom() plus _pinToBottom() both read a
    // bottom that no longer exists — bottom-follow latches off (observed live on a
    // parked deck pane: scrollTop froze at the park-time maximum while rows kept
    // arriving). Only the tail grows, so only the tail opts out; every sealed
    // block stays skipped, which is what keeps deep-history layout O(blocks).
    const cv = (el: FakeEl): string | undefined =>
      (el.style as unknown as Record<string, string>)["content-visibility"];
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const sb: FakeEl = sbEl(c);
    seedHeldHistory(r, 80, [row(0, "v")], seqN(7).map((i) => row(i, `s${i}`)));
    expect(cv(sb.children[0])).toBe("visible");

    const append = seqN(BLOCK).map((k) => row(7 + k, `s${7 + k}`));
    r.apply({ ...deltaFrame(80, 1, [row(0, "v")], append, 3), scrollbackTotal: 7 + BLOCK });
    expect(sb.children.length).toBe(2);
    expect(cv(sb.children[0])).toBeUndefined(); // sealed → back to the stylesheet's auto
    expect(cv(sb.children[1])).toBe("visible"); // the new open tail
  });

  test("the placeholder is a bare length — never the self-correcting `auto` form", () => {
    // `auto <length>` makes the browser REMEMBER a block's last rendered size and
    // use that instead of this value on every later skip, so a block that grows
    // while skipped (a parked deck pane's open tail block) keeps a stale height
    // and understates scrollHeight until it materializes — which moves the scroll
    // maximum out from under a bottom-pinned pane on reveal. rows × the measured
    // row height is already exact; there is nothing to self-correct.
    expect(blockPlaceholder(250, 16)).toBe("4000.00px");
    // rowH <= 0 (no layout yet) falls back to the em-derived default, never 0.
    expect(blockPlaceholder(10, 0)).toBe("168.00px");
  });
});
