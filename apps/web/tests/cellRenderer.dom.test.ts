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
import { CellGridRenderer, mergeFullFrame, rowText, blockPlaceholder, MAX_HELD_SCROLLBACK_ROWS } from "../src/lib/cellRenderer.ts";
import { DEFAULT_COLOR, type CellGridFrame, type CellRow } from "@roost/shared/cell";

// ── minimal fake DOM ──────────────────────────────────────────────────────
class FakeStyle {
  [key: string]: unknown;
  setProperty(k: string, v: string) { Object.defineProperty(this, k, { configurable: true, enumerable: true, value: v, writable: true }); }
  removeProperty(k: string) {
    const previous = typeof this[k] === "string" ? this[k] : "";
    Reflect.deleteProperty(this, k);
    return previous;
  }
}
// Layout model. The fake derives geometry from the painted rows so tests can
// distinguish browser geometry from CellGridRenderer's one conditional writer.
const PAD_TOP = 12; // .wterm padding-top (styles/sidebar.css)
const ROW_PX = 16;  // one .cell-row line box

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
  // Scroll surface. scrollHeight is derived from the painted rows, like browser
  // layout. scrollTop's setter counts application writes after each test resets
  // it; direct setup writes are reset before assertions.
  get scrollHeight(): number { return PAD_TOP + this.paintedRows * ROW_PX; }
  get paintedRows(): number {
    // The history spacer's reserved height IS scroll space (CellGridRenderer
    // ._syncSpacer) — count it as rows so the fake's geometry stays truthful.
    if (this.className === "cell-sb-spacer") return (parseFloat(String(this.style.height ?? "0")) || 0) / ROW_PX;
    if (this.className === "cell-row") return 1;
    let n = 0;
    for (const c of this.children) n += (c as FakeEl).paintedRows ?? 0;
    return n;
  }
  private _scrollTop = 0;
  scrollTopWrites = 0;
  get scrollTop(): number { return this._scrollTop; }
  set scrollTop(value: number) { this._scrollTop = value; this.scrollTopWrites++; }
  resetScrollTopWrites(): void { this.scrollTopWrites = 0; }
  clientHeight = 500;
  // scrollbackEl's offset inside .wterm (its offset parent). A preceding
  // .cell-sb-spacer sibling pushes it down, exactly as in real layout — which
  // is what makes nearHistoryTop() true for a reader inside reserved space.
  get offsetTop(): number {
    if (this.className !== "cell-scrollback") return PAD_TOP;
    const sp = (this.parentElement?.children as FakeEl[] | undefined)?.find((x) => x.className === "cell-sb-spacer");
    return PAD_TOP + (sp ? parseFloat(String(sp.style.height ?? "0")) || 0 : 0);
  }
  // rowHeight()'s hidden probe measures one .cell-row.
  getBoundingClientRect() { return { height: ROW_PX, width: 80, top: 0, left: 0, bottom: ROW_PX, right: 80 }; }
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
// Exactly the Document surface CellGridRenderer touches.
interface FakeDoc {
  createElement(tag: string): FakeEl;
  createTextNode(text: string): { textContent: string; parentElement: FakeEl | null };
  createDocumentFragment(): FakeEl;
  fonts: { ready: Promise<void> };
}
function makeContainer(): FakeEl {
  const doc: FakeDoc = {
    createElement: (t: string) => new FakeEl(t, doc),
    createTextNode: (s: string) => ({ textContent: s, parentElement: null }),
    createDocumentFragment: () => new FakeEl("#fragment", doc),
    // The constructor's late-webfont-swap hook (invalidates the cached row height).
    fonts: { ready: Promise.resolve() },
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
const sbEl = (c: FakeEl): FakeEl => c.children.find((x: FakeEl) => x.className === "cell-scrollback") as FakeEl;
const vpEl = (c: FakeEl): FakeEl => c.children.find((x: FakeEl) => x.className === "cell-viewport") as FakeEl;
const sbRows = (scrollbackEl: FakeEl): FakeEl[] =>
  scrollbackEl.children.flatMap((b: FakeEl) => b.children) as FakeEl[];

describe("CellGridRenderer DOM — append-only scrollback, no reflow", () => {
  test("a delta APPENDS scrollback; existing rows keep their identity", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);
    const scrollbackEl = sbEl(c); // ctor appends scrollback then viewport

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

  // heldFrameSeq is what a viewport claim reports (CellTerminal sendClaim /
  // sendBackgroundClaim → held_cell_seq), and the worker skips its claim
  // snapshot on a match. It MUST track the last APPLIED frame across deltas —
  // reporting a stale seq costs a redundant repaint, reporting one the viewer
  // never applied would suppress a repaint it needs.
  test("heldFrameSeq tracks the last applied frame across a full frame then deltas", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement); // FakeEl covers the renderer's DOM surface
    expect(r.heldFrameSeq()).toBe(0); // nothing held → the worker must snapshot

    r.apply(fullFrame(80, [row(0, "v0")], [row(0, "h0")]));
    expect(r.heldFrameSeq()).toBe(1); // fullFrame() carries seq 1

    r.apply(deltaFrame(80, 1, [row(0, "v0b")], [row(1, "h1")], 7));
    expect(r.heldFrameSeq()).toBe(7);

    r.apply(deltaFrame(80, 1, [row(0, "v0c")], [], 8));
    expect(r.heldFrameSeq()).toBe(8);
  });

  test("a viewport-only delta does NOT touch scrollback DOM at all", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);
    const scrollbackEl = sbEl(c);

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
    const viewportEl = vpEl(c); // ctor appends scrollback then viewport
    const scrollbackEl = sbEl(c);

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
    const viewportEl = vpEl(c);

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
    const viewportEl = vpEl(c);

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
    const scrollbackEl = sbEl(c);

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

    // A fullscreen terminal app enters altScreen:true → alt-active latches (CSS hides
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
    const viewportEl = vpEl(c);
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
    const viewportEl = vpEl(c);
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
    const viewportEl = vpEl(c);
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
    const viewportEl = vpEl(c);
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
    const scrollbackEl = sbEl(c);
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
    const scrollbackEl = sbEl(c);
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
    const scrollbackEl = sbEl(c);
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
    const scrollbackEl = sbEl(c);
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

  test("returning to the bottom re-enables held-window eviction", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    r.apply(fullFrame(80, [row(0, "v")], seq(100).map((i) => row(i, `h${i}`))));
    c.scrollTop = c.scrollHeight - c.clientHeight - ROW_PX;
    c.resetScrollTopWrites();
    const { idx } = grow(r, 100, 8);
    expect(r.currentFrame!.scrollbackRows.length).toBe(100 + 8 * BLOCK);
    expect(r.currentFrame!.scrollbackRows.length).toBeGreaterThan(MAX_HELD_SCROLLBACK_ROWS);
    expect(c.scrollTopWrites).toBe(0);

    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();
    r.apply(appDelta([row(idx, `s${idx}`)], idx + 1, 99));
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

  test("only the mutable tail is excluded from browser anchoring", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = sbEl(c);
    r.apply(tailFrame(80, [row(0, "v")], seq(300).map((i) => row(300 + i, `h${i}`)), 600));
    expect((scrollbackEl.children[0] as FakeEl).style["overflow-anchor"]).toBeUndefined();
    expect((scrollbackEl.children[1] as FakeEl).style["overflow-anchor"]).toBe("none");

    r.prependScrollback([row(299, "backfill")]);
    expect((scrollbackEl.children[2] as FakeEl).style["overflow-anchor"]).toBeUndefined();

    r.apply(appDelta([row(600, "stream")], 601, 2));
    expect((scrollbackEl.children[2] as FakeEl).style["overflow-anchor"]).toBe("none");
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
  test("eviction leaves an inspected viewport untouched", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    c.clientHeight = 500;
    r.apply(fullFrame(80, [row(0, "v")], seq(2000).map((i) => row(i, `h${i}`))));
    c.scrollTop = PAD_TOP + 800 * ROW_PX;
    const held = c.scrollTop;
    const baseBefore = r.currentFrame!.sbBase;
    const append = seq(BLOCK).map((k) => row(2000 + k, `s${2000 + k}`));
    c.resetScrollTopWrites();
    r.apply(appDelta(append, 2000 + BLOCK, 99));
    expect(r.currentFrame!.sbBase).toBe(baseBefore);
    expect(r.currentFrame!.scrollbackRows.length).toBe(2000 + BLOCK);
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
    r.apply(fullFrame(80, [row(0, "v")], seqN(7).map((i) => row(i, `s${i}`))));
    expect(sb.children.length).toBe(1);
    expect(csz(sb.children[0])).toBe("112.00px");   // 7 rows × 16px

    // Cross a block boundary: the closed block is exactly SB_BLOCK rows, the new
    // open block carries the remainder.
    const append = seqN(BLOCK).map((k) => row(7 + k, `s${7 + k}`));
    r.apply({ ...deltaFrame(80, 1, [row(0, "v")], append, 2), scrollbackTotal: 7 + BLOCK });
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
    r.apply(fullFrame(80, [row(0, "v")], seqN(7).map((i) => row(i, `s${i}`))));
    expect(cv(sb.children[0])).toBe("visible");

    const append = seqN(BLOCK).map((k) => row(7 + k, `s${7 + k}`));
    r.apply({ ...deltaFrame(80, 1, [row(0, "v")], append, 2), scrollbackTotal: 7 + BLOCK });
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

// ── bottom-only scrolling ─────────────────────────────────────────────────
// The renderer samples literal-bottom geometry before a mutation. It writes the
// new maximum only for that one case; native browser scrolling and anchoring own
// every other position.
describe("CellGridRenderer DOM — bottom-only scrolling", () => {
  const nRows = (n: number, from = 0) =>
    Array.from({ length: n }, (_, i) => row(from + i, `s${from + i}`));
  const appDelta = (append: CellRow[], total: number, seq: number): CellGridFrame =>
    ({ ...deltaFrame(80, 1, [row(0, "v")], append, seq), scrollbackTotal: total });

  test("a frame appended at the exact bottom pins to the new bottom", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    r.apply(fullFrame(80, [row(0, "v")], nRows(400)));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();

    r.apply(appDelta([row(400, "new")], 401, 2));

    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
    expect(r.atBottom()).toBe(true);
  });


  test("one pixel above the bottom is not pinned by a streaming frame", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    r.apply(fullFrame(80, [row(0, "v")], nRows(400)));
    c.scrollTop = c.scrollHeight - c.clientHeight - 1;
    const before = c.scrollTop;
    c.resetScrollTopWrites();

    r.apply(appDelta([row(400, "new")], 401, 2));

    expect(c.scrollTop).toBe(before);
    expect(c.scrollTopWrites).toBe(0);
    expect(r.atBottom()).toBe(false);
  });

  test("a non-bottom backfill prepend performs no application scroll write", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const total = 1000, held = 300;
    r.apply(tailFrame(80, [row(0, "v")], nRows(held, total - held), total));
    c.scrollTop = PAD_TOP + 50 * ROW_PX;
    const before = c.scrollTop;
    c.resetScrollTopWrites();

    r.prependScrollback(nRows(100, total - held - 100));

    expect(c.scrollTop).toBe(before);
    expect(c.scrollTopWrites).toBe(0);
    expect(r.backfillAnchor()!.sbBase).toBe(total - held - 100);
    expect(sbRows(sbEl(c)).length).toBe(held + 100);
    expect(r.currentFrame!.scrollbackRows.length).toBe(held + 100);
  });

  test("entering alt-screen from history performs no application scroll write", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    r.apply(fullFrame(80, [row(0, "v")], nRows(400)));
    c.scrollTop = PAD_TOP + 50 * ROW_PX;
    c.resetScrollTopWrites();

    r.apply(altFullFrame(80, [row(0, "TUI")], []));

    expect(c.classList.contains("alt-active")).toBe(true);
    expect(c.scrollTopWrites).toBe(0);
  });

  test("leaving alt-screen pins because the alt viewport is at bottom", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    r.apply(fullFrame(80, [row(0, "v")], nRows(400)));
    c.scrollTop = PAD_TOP + 50 * ROW_PX;
    r.apply(altFullFrame(80, [row(0, "TUI")], []));
    // CSS makes the alt viewport non-scrollable; model its browser clamp.
    c.scrollTop = 0;
    c.resetScrollTopWrites();

    r.apply(fullFrame(80, [row(0, "back")], nRows(400)));

    expect(c.classList.contains("alt-active")).toBe(false);
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
    expect(r.atBottom()).toBe(true);
  });

  // Why parking a deck pane at its own leaf's rect (TerminalDeck termStyle) is
  // the fix for "switching tabs loses the live bottom": the renderer is correct
  // in BOTH cases below. It keeps following the bottom exactly as long as the
  // scroll box's height doesn't change under it — a parked pane that keeps
  // applying frames at a DIFFERENT height moves its own scroll maximum away from
  // scrollTop, atBottom() latches false, and _pinToBottom (plus
  // _evictScrollback) is dead for that pane until the user scrolls to the end.
  test("frames applied while the box height is unchanged stay pinned to the live bottom", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    r.apply(fullFrame(80, [row(0, "v")], nRows(400)));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();

    for (let k = 0; k < 3; k++)
      r.apply(appDelta(nRows(50, 400 + 50 * k), 450 + 50 * k, 2 + k));

    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(r.atBottom()).toBe(true);
    expect(c.scrollTopWrites).toBe(3); // one pin per frame — single-writer contract
  });

  test("frames applied after the box height changed are not pinned", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    r.apply(fullFrame(80, [row(0, "v")], nRows(400)));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();
    // Shrinking the box (the 800×600 park under a taller pane) raises the scroll
    // maximum above the held scrollTop → the pre-mutation atBottom() reads false.
    c.clientHeight = 400;

    for (let k = 0; k < 3; k++)
      r.apply(appDelta(nRows(50, 400 + 50 * k), 450 + 50 * k, 2 + k));

    expect(c.scrollTopWrites).toBe(0);
    expect(r.atBottom()).toBe(false);
  });
});

// ── truthful scroll space: the [0, sbBase) history spacer ─────────────────
// A full frame ships only a scrollback TAIL, so the painted DOM used to occupy
// the WHOLE scroll space while describing ~250 rows: every backfill prepend
// grew scrollHeight (thumb shrank + jumped with no user action) and a reframe's
// replaceChildren left the browser's pixel offset over completely different
// rows (the "scrollbar all over the place after a tab switch" report).
// .cell-sb-spacer reserves the unpainted history, so an absolute row index has
// a FIXED pixel offset for the epoch and native scrollTop preserves the
// reader's row across prepend / evict / reframe with ZERO scroll writes.
describe("CellGridRenderer DOM — truthful scroll space", () => {
  const spEl = (c: FakeEl): FakeEl => c.children.find((x: FakeEl) => x.className === "cell-sb-spacer") as FakeEl;
  const spPx = (c: FakeEl): number => parseFloat(String(spEl(c).style.height));
  const nRows = (n: number, from = 0) =>
    Array.from({ length: n }, (_, i) => row(from + i, `r${from + i}`));
  const appDelta = (append: CellRow[], total: number, seq: number): CellGridFrame =>
    ({ ...deltaFrame(80, 1, [row(0, "v")], append, seq), scrollbackTotal: total });
  // The painted row the reader's pixel offset lands on, by GEOMETRY — the one
  // question the smoke helpers can't answer and the bug was hiding in.
  const rowAtReader = (c: FakeEl): string | undefined => {
    const sb = sbEl(c);
    const i = Math.floor((c.scrollTop - sb.offsetTop) / ROW_PX);
    return sbRows(sb)[i]?.children[0]?.textContent;
  };

  test("the spacer reserves the unpainted history", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    r.apply(tailFrame(80, [row(0, "v")], nRows(250, 500), 750));

    expect(spEl(c).style.height).toBe("8000.00px"); // 500 unpainted rows × 16px
    // 750 rows of history in the scroll space, not the 250 that are painted.
    expect(c.scrollHeight).toBe(PAD_TOP + (750 + 1) * ROW_PX); // +1 viewport row
    // A reader in reserved space is "near the painted top" → the drain pulls to them.
    c.scrollTop = PAD_TOP + 100 * ROW_PX;
    expect(r.nearHistoryTop()).toBe(true);
  });

  test("a backfill prepend shrinks the spacer by exactly the rows it adds", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    r.apply(tailFrame(80, [row(0, "v")], nRows(250, 500), 750));
    c.scrollTop = PAD_TOP + 600 * ROW_PX; // off the bottom, inside painted history
    const heightBefore = c.scrollHeight;
    const readerBefore = rowAtReader(c);
    c.resetScrollTopWrites();

    r.prependScrollback(nRows(250, 250));

    expect(spEl(c).style.height).toBe("4000.00px"); // 8000 - 250×16
    expect(c.scrollHeight).toBe(heightBefore);      // the thumb does not move
    expect(c.scrollTopWrites).toBe(0);
    expect(rowAtReader(c)).toBe(readerBefore);      // same absolute row, same offset
    expect(readerBefore).toBe("r600");
  });

  test("an eviction grows the spacer by exactly the rows it drops", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const held = 1900;
    r.apply(tailFrame(80, [row(0, "v")], nRows(held, 500), 500 + held));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight); // literal bottom
    const heightBefore = c.scrollHeight;
    const spacerBefore = spPx(c);

    r.apply(appDelta(nRows(250, 500 + held), 750 + held, 2)); // held 2150 > cap

    const dropped = r.currentFrame!.sbBase - 500; // rows the evictor pushed back into the hole
    expect(dropped).toBe(250);
    expect(r.currentFrame!.scrollbackRows.length).toBeLessThanOrEqual(MAX_HELD_SCROLLBACK_ROWS);
    expect(spPx(c)).toBe(spacerBefore + dropped * ROW_PX);
    // Net scroll space = the 250 rows that arrived. The eviction itself is free:
    // the spacer absorbs every dropped row, so no row above the reader moves.
    expect(c.scrollHeight).toBe(heightBefore + 250 * ROW_PX);
  });

  test("a reframe keeps the reader on the same absolute row", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    r.apply(tailFrame(80, [row(0, "v")], nRows(250, 500), 750));
    c.scrollTop = PAD_TOP + 600 * ROW_PX; // absolute row 600, by construction
    expect(rowAtReader(c)).toBe("r600");
    c.resetScrollTopWrites();

    // Slow path: a cols change makes mergeFullFrame return null → renderFull's
    // replaceChildren repaints a DIFFERENT tail (sbBase 400) from scratch.
    r.apply({ ...tailFrame(100, [row(0, "v")], nRows(400, 400), 800), seq: 3 });

    expect(spEl(c).style.height).toBe("6400.00px"); // 400 × 16
    expect(c.scrollTopWrites).toBe(0);              // row 120's single writer stays silent
    expect(rowAtReader(c)).toBe("r600");            // the tab-switch guarantee
  });
});
