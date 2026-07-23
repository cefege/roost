// CellGridRenderer DOM tripwire — the corruption-class guarantee in CI.
//
// Cell mode kills the terminal-history-corruption saga by NEVER re-parsing or
// reflowing: scrollback rows are immutable + append-only, and the painted
// width is pinned to the worker's grid cols (letterbox, no client reflow).
// Until now that guarantee was only checked live via /roost-smoke + the manual
// roost-render-stress skill — a /simplify pass that turned apply()'s delta path
// back into a full re-render would go green. This locks it by NODE IDENTITY:
// existing scrollback DOM nodes must survive every delta.
//
// No jsdom (by design, per cellRenderer.test.ts). A ~40-line fake DOM covers
// exactly what CellGridRenderer touches — node identity is all we assert.

import { describe, test, expect } from "bun:test";
import { CellGridRenderer, mergeFullFrame, rowText, MAX_HELD_SCROLLBACK_ROWS } from "../src/lib/cellRenderer.ts";
import { DEFAULT_COLOR, type CellGridFrame, type CellRow } from "@roost/shared/cell";

// ── minimal fake DOM ──────────────────────────────────────────────────────
class FakeStyle {
  setProperty(k: string, v: string) { (this as any)[k] = v; }
}
class FakeEl {
  className = "";
  children: any[] = [];
  style = new FakeStyle();
  dataset: Record<string, string> = {};
  attrs: Record<string, string> = {};
  textContent = "";
  parentElement: FakeEl | null = null;
  // Track class membership so tests can assert toggle() (alt-screen gating).
  private _classes = new Set<string>();
  classList = {
    add: (...c: string[]) => { for (const x of c) this._classes.add(x); },
    toggle: (c: string, on?: boolean) => {
      const want = on ?? !this._classes.has(c);
      if (want) this._classes.add(c); else this._classes.delete(c);
      return want;
    },
    contains: (c: string) => this._classes.has(c),
  };
  constructor(public tagName: string, public ownerDocument: any) {}
  appendChild(c: any) { c.parentElement = this; this.children.push(c); return c; }
  prepend(...kids: FakeEl[]) {
    // DocumentFragment-style: prepending a fake fragment splices its children.
    const flat = kids.flatMap((k) => (k.tagName === "#fragment" ? k.children as FakeEl[] : [k]));
    for (const k of flat) k.parentElement = this;
    this.children.unshift(...flat);
  }
  // Scroll surface for prependScrollback's distance-from-bottom math; plain
  // numbers are enough for node-identity assertions.
  scrollHeight = 0;
  scrollTop = 0;
  replaceChildren(...kids: any[]) {
    for (const c of this.children) c.parentElement = null;
    this.children = kids.slice();
    for (const k of kids) k.parentElement = this;
  }
  insertBefore(c: FakeEl, ref: FakeEl | null) {
    c.parentElement = this;
    const i = ref ? this.children.indexOf(ref) : -1;
    if (i >= 0) this.children.splice(i, 0, c); else this.children.push(c);
    return c;
  }
  replaceWith(next: FakeEl) {
    const p = this.parentElement;
    if (!p) return;
    const i = p.children.indexOf(this);
    if (i >= 0) { p.children[i] = next; next.parentElement = p; this.parentElement = null; }
  }
  setAttribute(k: string, v: string) { this.attrs[k] = v; }
  remove() {
    const p = this.parentElement;
    if (p) { const i = p.children.indexOf(this); if (i >= 0) p.children.splice(i, 1); }
    this.parentElement = null;
  }
  get firstElementChild(): FakeEl | null { return this.children[0] ?? null; }
}
function makeContainer(): FakeEl {
  const doc: any = {
    createElement: (t: string) => new FakeEl(t, doc),
    createTextNode: (s: string) => ({ textContent: s, parentElement: null }),
    createDocumentFragment: () => new FakeEl("#fragment", doc),
  };
  return new FakeEl("div", doc);
}

// ── frame builders ────────────────────────────────────────────────────────
function row(index: number, text: string): CellRow {
  return { index, spans: text ? [{ text, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0 }] : [] };
}
const BASE = { cursorRow: 0, cursorCol: 0, cursorVisible: true, altScreen: false, cursorKeysApp: false, bracketedPaste: false } as const;
function fullFrame(cols: number, viewport: CellRow[], scrollback: CellRow[]): CellGridFrame {
  return { ...BASE, cols, rows: viewport.length, full: true,
    viewportRows: viewport, scrollbackRows: scrollback, scrollbackAppend: [],
    scrollbackTotal: scrollback.length, sbBase: 0, seq: 1 };
}
function deltaFrame(cols: number, rows: number, viewport: CellRow[], append: CellRow[], seq: number): CellGridFrame {
  return { ...BASE, cols, rows, full: false,
    viewportRows: viewport, scrollbackRows: [], scrollbackAppend: append,
    scrollbackTotal: 0, sbBase: 0, seq };
}
// Tail full frame (lazy-history attach): scrollback carries only rows
// [sbBase, total); [0, sbBase) is the backfill hole.
function tailFrame(cols: number, viewport: CellRow[], tail: CellRow[], total: number): CellGridFrame {
  return { ...BASE, cols, rows: viewport.length, full: true,
    viewportRows: viewport, scrollbackRows: tail, scrollbackAppend: [],
    scrollbackTotal: total, sbBase: total - tail.length, seq: 2 };
}
// Alt-screen variants: same shape, altScreen flipped on.
function altFullFrame(cols: number, viewport: CellRow[], scrollback: CellRow[]): CellGridFrame {
  return { ...fullFrame(cols, viewport, scrollback), altScreen: true };
}
function altDeltaFrame(cols: number, rows: number, viewport: CellRow[], seq: number): CellGridFrame {
  return { ...deltaFrame(cols, rows, viewport, [], seq), altScreen: true };
}

// Scrollback rows are packed into .cell-block wrappers (SB_BLOCK content-
// visibility blocks — WIP block packing); flatten to assert ROW identity.
const sbRows = (scrollbackEl: FakeEl): FakeEl[] =>
  scrollbackEl.children.flatMap((b: FakeEl) => b.children) as FakeEl[];

describe("CellGridRenderer DOM — append-only scrollback, no reflow", () => {
  test("a delta APPENDS scrollback; existing rows keep their identity", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);
    const scrollbackEl = c.children[0]; // ctor appends scrollback then viewport

    r.apply(fullFrame(80, [row(0, "v0"), row(1, "v1")], [row(0, "h0"), row(1, "h1")]));
    const rows0 = sbRows(scrollbackEl);
    expect(rows0.length).toBe(2);
    const h0 = rows0[0];
    const h1 = rows0[1];

    r.apply(deltaFrame(80, 2, [row(1, "v1b")], [row(2, "h2")], 2));
    // Append-only: the two original nodes are the SAME objects (not re-rendered),
    // the third is new. A full re-render would replace all three.
    const rows1 = sbRows(scrollbackEl);
    expect(rows1.length).toBe(3);
    expect(rows1[0]).toBe(h0);
    expect(rows1[1]).toBe(h1);
    expect(rows1[2]).not.toBe(h1);
  });

  test("a viewport-only delta does NOT touch scrollback DOM at all", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);
    const scrollbackEl = c.children[0];

    r.apply(fullFrame(80, [row(0, "v0")], [row(0, "h0"), row(1, "h1")]));
    const before = sbRows(scrollbackEl);

    r.apply(deltaFrame(80, 1, [row(0, "v0-changed")], [], 2));
    const after = sbRows(scrollbackEl);
    expect(after.length).toBe(2);
    expect(after[0]).toBe(before[0]);
    expect(after[1]).toBe(before[1]);
  });

  test("painted width is pinned to the worker's frame.cols (letterbox, no reflow)", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);

    r.apply(fullFrame(80, [row(0, "v0")], []));
    expect((c.style as any)["--cell-cols"]).toBe("80");
    // The renderer has no container-width input — a delta can only carry the
    // worker's cols, never the pane width. Cols stays the worker's value.
    r.apply(deltaFrame(80, 1, [row(0, "x")], [], 2));
    expect((c.style as any)["--cell-cols"]).toBe("80");
  });

  test("selection hold freezes viewport DOM, then flushes the latest frame on release", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);
    const viewportEl = c.children[1]; // ctor appends scrollback then viewport
    const scrollbackEl = c.children[0];

    r.apply(fullFrame(80, [row(0, "v0")], [row(0, "h0")]));
    const heldRow = viewportEl.children[0]; // the painted viewport row node

    // Selection active → frames fold into state but the DOM must NOT change,
    // or the browser would drop the user's in-progress selection.
    r.setSelectionHold(true);
    r.apply(deltaFrame(80, 1, [row(0, "v0-changed")], [row(1, "h1")], 2));
    r.apply(deltaFrame(80, 1, [row(0, "v0-again")], [], 3));
    expect(viewportEl.children[0]).toBe(heldRow);   // viewport frozen
    expect(sbRows(scrollbackEl).length).toBe(1);    // scrollback append deferred

    // Release → reconcile to the LATEST folded frame (v0-again + h1 appended).
    r.setSelectionHold(false);
    expect(viewportEl.children[0]).not.toBe(heldRow); // repainted
    expect(sbRows(scrollbackEl).length).toBe(2);      // deferred append applied
  });

  test("armed hold (Cmd-hover a link) freezes viewport, then flushes on release", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);
    const viewportEl = c.children[1];

    r.apply(fullFrame(80, [row(0, "v0")], []));
    const heldRow = viewportEl.children[0];

    // Cmd-held over the pane → the linkifier holds repaints so the wrapped <a>
    // isn't rebuilt out from under the cursor (the pointer↔text flicker).
    r.setArmedHold(true);
    r.apply(deltaFrame(80, 1, [row(0, "v0-changed")], [], 2));
    expect(viewportEl.children[0]).toBe(heldRow); // frozen — anchor survives

    r.setArmedHold(false);
    expect(viewportEl.children[0]).not.toBe(heldRow); // repainted to latest
  });

  test("holds compose: releasing one while the other is active stays frozen", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);
    const viewportEl = c.children[1];

    r.apply(fullFrame(80, [row(0, "v0")], []));
    const heldRow = viewportEl.children[0];

    r.setSelectionHold(true);
    r.setArmedHold(true);
    r.apply(deltaFrame(80, 1, [row(0, "v0-changed")], [], 2));
    expect(viewportEl.children[0]).toBe(heldRow);

    // Release selection — armed still holds → must stay frozen (a premature flush
    // here would re-churn the link anchor the user is about to click).
    r.setSelectionHold(false);
    expect(viewportEl.children[0]).toBe(heldRow);

    // Release the last hold → flush to latest.
    r.setArmedHold(false);
    expect(viewportEl.children[0]).not.toBe(heldRow);
  });

  test("a delta before any full frame is dropped (self-heals on the next full)", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);
    const scrollbackEl = c.children[0];

    r.apply(deltaFrame(80, 1, [row(0, "x")], [row(0, "orphan")], 1));
    expect(scrollbackEl.children.length).toBe(0); // no base frame → dropped

    r.apply(fullFrame(80, [row(0, "v0")], [row(0, "h0")]));
    expect(scrollbackEl.children.length).toBe(1);
  });

  test("alt-screen frame sets .alt-active; leaving alt clears it", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);

    // Main-screen full frame → no alt-active (scrollback stays visible/scrollable).
    r.apply(fullFrame(80, [row(0, "v0")], [row(0, "h0")]));
    expect(c.classList.contains("alt-active")).toBe(false);

    // Claude enters fullscreen → altScreen:true → alt-active latches (CSS hides
    // scrollback + locks scroll: no historic junk on top, no scroll-up).
    r.apply(altFullFrame(80, [row(0, "TUI")], [row(0, "stale-h0")]));
    expect(c.classList.contains("alt-active")).toBe(true);

    // A delta while still in alt keeps it on.
    r.apply(altDeltaFrame(80, 1, [row(0, "TUI2")], 3));
    expect(c.classList.contains("alt-active")).toBe(true);

    // Leaving alt (main-screen frame) clears it → scrollback returns.
    r.apply(fullFrame(80, [row(0, "back")], [row(0, "h0")]));
    expect(c.classList.contains("alt-active")).toBe(false);
  });
});

// ── viewport diff — unchanged rows keep node identity (idle-churn kill) ──
// renderViewport re-renders only rows whose rowSig changed. A regression back
// to full replaceChildren re-renders every row per frame (the deck-wide idle
// churn: ~1.5k nodes/3s per quiet pane) and flips these node-identity checks.
describe("CellGridRenderer DOM — viewport diff", () => {
  test("a content-identical delta touches NO viewport row nodes", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement); // FakeEl covers the renderer's DOM surface
    const viewportEl = c.children[1];
    r.apply(fullFrame(80, [row(0, "v0"), row(1, "v1")], []));
    const n0 = viewportEl.children[0];
    const n1 = viewportEl.children[1];
    r.apply(deltaFrame(80, 2, [row(0, "v0"), row(1, "v1")], [], 2)); // cursor-only style delta
    expect(viewportEl.children[0]).toBe(n0);
    expect(viewportEl.children[1]).toBe(n1);
  });

  test("a one-row delta replaces ONLY that row's node, positionally", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = c.children[1];
    r.apply(fullFrame(80, [row(0, "v0"), row(1, "v1")], []));
    const n0 = viewportEl.children[0];
    const n1 = viewportEl.children[1];
    r.apply(deltaFrame(80, 2, [row(1, "v1-changed")], [], 2));
    expect(viewportEl.children[0]).toBe(n0); // untouched → zero DOM writes
    expect(viewportEl.children[1]).not.toBe(n1); // replaced in place
    expect(viewportEl.children[1].children[0].textContent).toBe("v1-changed");
  });

  test("a full fast-append frame with fewer viewport rows prunes the tail", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = c.children[1];
    r.apply(fullFrame(80, [row(0, "v0"), row(1, "v1"), row(2, "v2")], [row(0, "h0")]));
    const n0 = viewportEl.children[0];
    expect(viewportEl.children.length).toBe(5); // 3 rows + cursor + ghosts overlays
    r.apply(fullFrame(80, [row(0, "v0")], [row(0, "h0")])); // same width/scrollback → fast path diff
    expect(viewportEl.children.length).toBe(3); // 1 row + overlays
    expect(viewportEl.children[0]).toBe(n0); // head row identity kept
  });

  test("a scrolling delta REUSES shifted row nodes; only the new tail renders", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = c.children[1];
    r.apply(fullFrame(80, [row(0, "A"), row(1, "B"), row(2, "C")], []));
    const nB = viewportEl.children[1];
    const nC = viewportEl.children[2];
    // One line scrolled out: A moved to scrollback, viewport is now B,C,D.
    r.apply(deltaFrame(80, 3, [row(0, "B"), row(1, "C"), row(2, "D")], [row(0, "A")], 2));
    expect(viewportEl.children[0]).toBe(nB); // shifted up, node reused
    expect(viewportEl.children[1]).toBe(nC); // shifted up, node reused
    expect(viewportEl.children[2]).not.toBe(nC); // the only newly rendered row
    expect(viewportEl.children[2].children[0].textContent).toBe("D");
  });
});

// ── lazy-history: tail full frames + backfill splice ─────────────────────
// Attach/reframe full frames carry only a SB_SNAPSHOT_TAIL_ROWS scrollback
// tail (sbBase > 0); the [0, sbBase) rest arrives via prependScrollback.
// These lock the two invariants: a broadcast tail frame never wipes an
// already-deep viewer (merge keeps node identity), and a backfill splice
// lands above without touching existing nodes.
describe("CellGridRenderer DOM — tail frames + backfill", () => {
  test("mergeFullFrame extends the held window from an overlapping tail", () => {
    const base = fullFrame(80, [row(0, "v")], [row(0, "h0"), row(1, "h1"), row(2, "h2")]);
    // Two lines scrolled off since; tail covers [2, 5) of total 5.
    const tail = tailFrame(80, [row(0, "v")], [row(2, "h2"), row(3, "h3"), row(4, "h4")], 5);
    const m = mergeFullFrame(base, tail);
    expect(m).not.toBeNull();
    expect(m!.appended.map((r) => r.index)).toEqual([3, 4]);
    expect(m!.frame.sbBase).toBe(0);
    expect(m!.frame.scrollbackRows.map((r) => r.index)).toEqual([0, 1, 2, 3, 4]);
  });

  test("mergeFullFrame rejects width change, shrink, gap, boundary mismatch", () => {
    const base = fullFrame(80, [row(0, "v")], [row(0, "h0"), row(1, "h1")]);
    const tail = (cols: number, rows2: CellRow[], total: number) =>
      tailFrame(cols, [row(0, "v")], rows2, total);
    expect(mergeFullFrame(base, tail(81, [row(1, "h1")], 2))).toBeNull(); // width
    expect(mergeFullFrame(base, { ...tail(80, [row(0, "h0")], 1) })).toBeNull(); // shrink
    expect(mergeFullFrame(base, tail(80, [row(4, "h4")], 5))).toBeNull(); // gap: tail starts past held
    expect(mergeFullFrame(base, tail(80, [row(1, "DIFFERENT"), row(2, "h2")], 3))).toBeNull(); // boundary text
    // held-nothing base only merges a complete (sbBase 0) frame
    const empty = fullFrame(80, [row(0, "v")], []);
    expect(mergeFullFrame(empty, tail(80, [row(3, "h3")], 4))).toBeNull();
  });

  test("a tail full frame fast-appends onto a deep viewer (node identity)", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = c.children[0];
    r.apply(fullFrame(80, [row(0, "v")], [row(0, "h0"), row(1, "h1"), row(2, "h2")]));
    const before = sbRows(scrollbackEl);
    // Another viewer attaches → broadcast tail frame [1, 4) of total 4.
    r.apply(tailFrame(80, [row(0, "v")], [row(1, "h1"), row(2, "h2"), row(3, "h3")], 4));
    const after = sbRows(scrollbackEl);
    expect(after.length).toBe(4);
    expect(after[0]).toBe(before[0]); // deep history NOT wiped
    expect(after[1]).toBe(before[1]);
    expect(after[2]).toBe(before[2]);
    expect(r.backfillAnchor()!.sbBase).toBe(0); // still complete
  });

  test("fresh mount on a tail frame leaves a hole; prependScrollback fills it above", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = c.children[0];
    r.apply(tailFrame(80, [row(0, "v")], [row(2, "h2"), row(3, "h3")], 4));
    const a0 = r.backfillAnchor()!;
    expect(a0.sbBase).toBe(2);
    expect(a0.firstHeldText).toBe("h2");
    const tailNodes = sbRows(scrollbackEl);
    expect(tailNodes.length).toBe(2);
    // Backfill splice [0, 2) — must land ABOVE, existing nodes untouched.
    r.prependScrollback([row(0, "h0"), row(1, "h1")]);
    const all = sbRows(scrollbackEl);
    expect(all.length).toBe(4);
    expect(all[0].children[0].textContent).toBe("h0");
    expect(all[2]).toBe(tailNodes[0]);
    expect(all[3]).toBe(tailNodes[1]);
    expect(r.backfillAnchor()!.sbBase).toBe(0);
    // Misaligned splice (epoch moved) is dropped, not applied.
    r.prependScrollback([row(0, "stale")]);
    expect(sbRows(scrollbackEl).length).toBe(4);
  });

  test("a delta after a merged tail keeps splicing appends correctly", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = c.children[0];
    r.apply(tailFrame(80, [row(0, "v")], [row(1, "h1")], 2));
    r.apply({ ...deltaFrame(80, 1, [], [row(2, "h2")], 3), scrollbackTotal: 3 });
    expect(sbRows(scrollbackEl).length).toBe(2); // h1 + h2 (hole [0,1) pending)
    expect(r.backfillAnchor()!.sbBase).toBe(1);
    r.prependScrollback([row(0, "h0")]);
    expect(sbRows(scrollbackEl).map((n) => n.children[0].textContent)).toEqual(["h0", "h1", "h2"]);
  });
});

// ── client-side eviction: cap the held scrollback window ─────────────────
// .cell-scrollback was append-only, so a long stable streaming session grew
// live DOM nodes ~500/min without bound (the long-uptime lag). _evictScrollback
// trims oldest whole content-visibility blocks once the held window exceeds
// MAX_HELD_SCROLLBACK_ROWS, bumping sbBase so the held-window invariant
// (scrollbackRows.length === scrollbackTotal - sbBase) stays honest and
// scrollbackBackfill re-pulls the evicted range on scroll-up. These lock the
// cap, the invariant + DOM↔array alignment, the freeze under a scrolled-up
// reader, mergeFullFrame after eviction, and — critically — that a PARTIAL
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
      r.apply(appDelta(append, total, i + 2));
    }
    return { total, idx };
  };

  test("eviction caps the held window and preserves invariant + DOM alignment", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = c.children[0];
    r.apply(fullFrame(80, [row(0, "v")], seq(100).map((i) => row(i, `h${i}`))));
    const { total, idx } = grow(r, 100, 12);
    // After every apply the invariant, the cap, and DOM↔array alignment hold.
    const f = r.currentFrame!;
    expect(f.scrollbackTotal - f.sbBase).toBe(f.scrollbackRows.length);
    expect(f.scrollbackRows.length).toBeLessThanOrEqual(MAX_HELD_SCROLLBACK_ROWS);
    expect(sbRows(scrollbackEl).length).toBe(f.scrollbackRows.length);
    // Block count bounded (≤ ceil(MAX_HELD / BLOCK) + 1 open tail block).
    expect(scrollbackEl.children.length).toBeLessThanOrEqual(Math.ceil(MAX_HELD_SCROLLBACK_ROWS / BLOCK) + 1);
    // Tail row survived eviction unchanged.
    expect(rowText(f.scrollbackRows[f.scrollbackRows.length - 1]!)).toBe(`s${idx - 1}`);
    expect(total).toBe(idx); // sanity: total tracks the last appended index
  });

  test("setEvictionFrozen(true) grows past the cap; thaw trims back on next append", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    r.apply(fullFrame(80, [row(0, "v")], seq(100).map((i) => row(i, `h${i}`))));
    r.setEvictionFrozen(true);
    const { idx } = grow(r, 100, 12); // frozen → no trim
    expect(r.currentFrame!.scrollbackRows.length).toBe(100 + 12 * BLOCK);
    expect(r.currentFrame!.scrollbackRows.length).toBeGreaterThan(MAX_HELD_SCROLLBACK_ROWS);
    // Thaw + one more append → trims back under the cap, invariant intact.
    r.setEvictionFrozen(false);
    const append = seq(BLOCK).map((k) => row(idx + k, `s${idx + k}`));
    r.apply(appDelta(append, idx + BLOCK, 99));
    const f = r.currentFrame!;
    expect(f.scrollbackRows.length).toBeLessThanOrEqual(MAX_HELD_SCROLLBACK_ROWS);
    expect(f.scrollbackTotal - f.sbBase).toBe(f.scrollbackRows.length);
  });

  test("a partial leading block (backfill prepend) never desyncs DOM from array", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = c.children[0];
    // Tail frame (sbBase > 0): held = last 100 rows of a 2500-row history.
    const total = 2500;
    const tailStart = total - 100;
    r.apply(tailFrame(80, [row(0, "v")], seq(100).map((k) => row(tailStart + k, `h${tailStart + k}`)), total));
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
      r.apply(appDelta(append, running, i + 2));
      const f = r.currentFrame!;
      expect(f.scrollbackTotal - f.sbBase).toBe(f.scrollbackRows.length);
      expect(f.scrollbackRows.length).toBeLessThanOrEqual(MAX_HELD_SCROLLBACK_ROWS);
      // Killer assertion: painted DOM row count must track the array. A hardcoded
      // dropped = SB_BLOCK would leave the 180-row block's worth of DOM behind
      // while slicing 250 off the array → DOM count > array length.
      expect(sbRows(scrollbackEl).length).toBe(f.scrollbackRows.length);
    }
  });

  test("after eviction, an extending tail full frame still merges; anchor base is honest", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    r.apply(fullFrame(80, [row(0, "v")], seq(100).map((i) => row(i, `h${i}`))));
    const { total, idx } = grow(r, 100, 12);
    const after = r.currentFrame!;
    expect(after.sbBase).toBeGreaterThan(0); // eviction actually bumped the base
    expect(r.backfillAnchor()!.sbBase).toBe(after.sbBase); // anchor reports bumped base
    // A tail overlapping the held window's last row + one new row must merge
    // (mergeFullFrame reads the held TAIL; eviction only ever drops the HEAD).
    const lastHeld = after.scrollbackRows[after.scrollbackRows.length - 1]!;
    const extTail = tailFrame(80, [row(0, "v")], [lastHeld, row(idx, `new${idx}`)], total + 1);
    expect(mergeFullFrame(after, extTail)).not.toBeNull();
  });
});
