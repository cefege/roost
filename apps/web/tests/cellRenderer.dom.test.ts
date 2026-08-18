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
import {
  CellGridRenderer,
  blockPlaceholder,
  MAX_HELD_SCROLLBACK_ROWS,
  RENDERER_HOLD_LINK,
  RENDERER_HOLD_SELECTION,
} from "../src/lib/cellRenderer.ts";
import { registerRenderer, terminalBrowserStreamSnapshot } from "../src/lib/terminalPreview.ts";
import { DEFAULT_COLOR, spansText, type CellGridFrame, type CellRow } from "@roost/shared/cell";

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
  nextScrollTopWriteResult: number | null = null;
  scrollTopWrites = 0;
  get scrollTop(): number { return this._scrollTop; }
  set scrollTop(value: number) {
    this._scrollTop = this.nextScrollTopWriteResult ?? value;
    this.nextScrollTopWriteResult = null;
    this.scrollTopWrites++;
  }
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
  return { index, spans: text ? [{ text, columns: text.length, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0 }] : [] };
}
const BASE = {
  gridEpoch: "test-grid:0",
  cursorRow: 0, cursorCol: 0, cursorVisible: true, altScreen: false,
  cursorKeysApp: false, bracketedPaste: false,
  mouseTracking: 0, mouseSgr: false, focusEvents: false,
} as const;
function fullFrame(cols: number, viewport: CellRow[], scrollbackTotal = 0): CellGridFrame {
  return { ...BASE, cols, rows: viewport.length, full: true,
    viewportRows: viewport, scrollbackRows: [], scrollbackAppend: [],
    scrollbackTotal, sbBase: scrollbackTotal, seq: 1 };
}
function deltaFrame(
  cols: number,
  rows: number,
  viewport: CellRow[],
  append: CellRow[],
  seq: number,
): CellGridFrame {
  return { ...BASE, cols, rows, full: false,
    viewportRows: viewport, scrollbackRows: [], scrollbackAppend: append,
    scrollbackTotal: 0, sbBase: 0, seq };
}
function seedHeldHistory(
  renderer: CellGridRenderer,
  cols: number,
  viewport: CellRow[],
  history: CellRow[],
  total = history.length,
): boolean {
  const baseTotal = history[0]?.index ?? total;
  if (!renderer.apply(fullFrame(cols, viewport, baseTotal))) return false;
  if (history.length === 0) return true;
  return renderer.apply({
    ...deltaFrame(cols, viewport.length, viewport, history, 2),
    scrollbackTotal: total,
  });
}
// Alt-screen transitions create a new semantic grid epoch.
function altFullFrame(cols: number, viewport: CellRow[], _history: CellRow[]): CellGridFrame {
  return { ...fullFrame(cols, viewport), gridEpoch: "test-grid:1", altScreen: true };
}
function altDeltaFrame(cols: number, rows: number, viewport: CellRow[], seq: number): CellGridFrame {
  return { ...deltaFrame(cols, rows, viewport, [], seq), gridEpoch: "test-grid:1", altScreen: true };
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

    seedHeldHistory(r, 80, [row(0, "v0"), row(1, "v1")], [row(0, "h0"), row(1, "h1")]);
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

  // heldFrameSeq is what a visible viewport claim reports as held_cell_seq.
  // It MUST track the last APPLIED frame across deltas: reporting a stale seq
  // costs a redundant repaint, while reporting one the viewer never applied
  // would suppress the authoritative reclaim snapshot it needs.
  test("heldFrameSeq tracks the last applied frame across a full frame then deltas", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement); // FakeEl covers the renderer's DOM surface
    expect(r.heldFrameSeq()).toBe(0); // nothing held → the worker must snapshot

    seedHeldHistory(r, 80, [row(0, "v0")], []);
    expect(r.heldFrameSeq()).toBe(1); // fullFrame() carries seq 1

    r.apply(deltaFrame(80, 1, [row(0, "v0b")], [row(1, "h1")], 7));
    expect(r.heldFrameSeq()).toBe(7);

    r.apply(deltaFrame(80, 1, [row(0, "v0c")], [], 8));
    expect(r.heldFrameSeq()).toBe(8);
  });

  test("a delta from a different grid epoch is rejected", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "held")], []);
    const stale = {
      ...deltaFrame(80, 1, [row(0, "wrong")], [], 2),
      gridEpoch: "test-grid:1",
    };
    expect(r.apply(stale)).toBe(false);
    expect(r.currentFrame?.seq).toBe(1);
    expect(r.gridText()).toBe("held");
  });

  test("a viewport-only delta does NOT touch scrollback DOM at all", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);
    const scrollbackEl = sbEl(c);

    seedHeldHistory(r, 80, [row(0, "v0")], [row(0, "h0"), row(1, "h1")]);
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

    seedHeldHistory(r, 80, [row(0, "v0")], []);
    expect((c.style as any)["--cell-cols"]).toBe("80");
    // The renderer has no container-width input — a delta can only carry the
    // worker's cols, never the pane width. Cols stays the worker's value.
    r.apply(deltaFrame(80, 1, [row(0, "x")], [], 2));
    expect((c.style as any)["--cell-cols"]).toBe("80");
  });

  test("selection enters reading; release stays frozen until admitted input resumes live", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    const scrollbackEl = sbEl(c);

    expect(seedHeldHistory(r, 80, [row(0, "v0")], [row(0, "h0")])).toBe(true);
    const beforeHold = r.reconciledEpochSeq();
    const heldRow = viewportEl.children[0];

    expect(r.setSelectionHold(true)).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("reading");
    expect(r.holdMask).toBe(RENDERER_HOLD_SELECTION);
    expect(r.apply({
      ...deltaFrame(80, 1, [row(0, "v0-changed")], [row(1, "h1")], 3),
      scrollbackTotal: 2,
    })).toBe(true);
    expect(r.apply({
      ...deltaFrame(80, 1, [row(0, "v0-again")], [], 4),
      scrollbackTotal: 2,
    })).toBe(true);
    expect(viewportEl.children[0]).toBe(heldRow);
    expect(sbRows(scrollbackEl).length).toBe(1);
    expect(r.canonicalEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 4 });
    expect(r.reconciledEpochSeq()).toEqual(beforeHold);
    expect(r.reconcileBlockReason()).toBe("reader_pending_frame");
    expect(r.presentationSnapshot().hold_mask).toEqual({ selection: true, link: false });
    expect(r.gridText()).toBe("v0-again");

    expect(r.setSelectionHold(false)).toEqual({ reconciled: false, anchorChanged: false });
    expect(viewportEl.children[0]).toBe(heldRow);
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(viewportEl.children[0]).not.toBe(heldRow);
    expect(sbRows(scrollbackEl).length).toBe(2);
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 4 });
    expect(r.readerIntent).toBe("live");
    expect(r.reconcileBlockReason()).toBeNull();
  });

  test("armed link hold freezes viewport and flushes on release without entering reading", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);

    seedHeldHistory(r, 80, [row(0, "v0")], []);
    const heldRow = viewportEl.children[0];

    r.setArmedHold(true);
    expect(r.readerIntent).toBe("live");
    expect(r.holdMask).toBe(RENDERER_HOLD_LINK);
    r.apply(deltaFrame(80, 1, [row(0, "v0-changed")], [], 2));
    expect(viewportEl.children[0]).toBe(heldRow);
    expect(r.canonicalEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 2 });
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 1 });
    expect(r.reconcileBlockReason()).toBe("link_hold");

    expect(r.setArmedHold(false)).toEqual({ reconciled: true, anchorChanged: false });
    expect(viewportEl.children[0]).not.toBe(heldRow);
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 2 });
    expect(r.reconcileBlockReason()).toBeNull();
  });

  test("selection and link holds clear atomically with at most one epoch repair", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);

    seedHeldHistory(r, 80, [row(0, "v0")], []);
    r.setSelectionHold(true);
    r.setArmedHold(true);
    expect(r.holdMask).toBe(RENDERER_HOLD_SELECTION | RENDERER_HOLD_LINK);
    expect(r.presentationSnapshot().hold_mask).toEqual({ selection: true, link: true });
    expect(r.apply({
      ...altFullFrame(80, [row(0, "TUI")], []),
      seq: 2,
    })).toBe(true);

    const originalReplace = viewportEl.replaceChildren.bind(viewportEl);
    let repairWrites = 0;
    viewportEl.replaceChildren = (...children: unknown[]) => {
      repairWrites++;
      originalReplace(...children);
    };
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(repairWrites).toBe(1);
    expect(r.holdMask).toBe(0);
    expect(r.readerIntent).toBe("live");
    expect(c.classList.contains("alt-active")).toBe(true);
    expect(r.reconcileBlockReason()).toBeNull();
  });

  test("a delta before any full frame is rejected and the next full is accepted", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = sbEl(c);

    expect(r.apply(deltaFrame(80, 1, [row(0, "x")], [row(0, "orphan")], 1))).toBe(false);
    expect(r.heldFrameSeq()).toBe(0);
    expect(scrollbackEl.children.length).toBe(0);

    expect(seedHeldHistory(r, 80, [row(0, "v0")], [row(0, "h0")])).toBe(true);
    expect(scrollbackEl.children.length).toBe(1);
  });

  test("alt-screen frame sets .alt-active; leaving alt clears it", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as any);

    // Main-screen full frame → no alt-active (scrollback stays visible/scrollable).
    seedHeldHistory(r, 80, [row(0, "v0")], [row(0, "h0")]);
    expect(c.classList.contains("alt-active")).toBe(false);

    // A fullscreen terminal app enters altScreen:true → alt-active latches (CSS hides
    // scrollback + locks scroll: no historic junk on top, no scroll-up).
    r.apply(altFullFrame(80, [row(0, "TUI")], [row(0, "stale-h0")]));
    expect(r.presentationSnapshot().mode).toEqual({
      canonical: {
        alt_screen: true,
        cursor_keys_app: false,
        bracketed_paste: false,
      },
      reconciled: {
        alt_screen: true,
        cursor_keys_app: false,
        bracketed_paste: false,
      },
    });
    expect(c.classList.contains("alt-active")).toBe(true);

    // A delta while still in alt keeps it on.
    r.apply(altDeltaFrame(80, 1, [row(0, "TUI2")], 3));
    expect(c.classList.contains("alt-active")).toBe(true);

    // Leaving alt (main-screen frame) clears it → scrollback returns.
    seedHeldHistory(r, 80, [row(0, "back")], [row(0, "h0")]);
    expect(c.classList.contains("alt-active")).toBe(false);
  });
});

// ── viewport patching — only authoritative dirty rows are inspected ──────
// A regression to full reconstruction/re-hashing either changes untouched
// node identity or trips the poisoned-row accessor below.
describe("CellGridRenderer DOM — viewport diff", () => {
  test("a content-identical delta advances reconciliation without replacing rows", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "v0"), row(1, "v1")], []);
    const n0 = viewportEl.children[0];
    const n1 = viewportEl.children[1];
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 1 });

    // Same row hashes, mode, and cursor: reconciliation is proven by the
    // completed diff/cursor/mode path even though it performs zero row writes.
    r.apply(deltaFrame(80, 2, [row(0, "v0"), row(1, "v1")], [], 2));
    expect(viewportEl.children[0]).toBe(n0);
    expect(viewportEl.children[1]).toBe(n1);
    expect(r.canonicalEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 2 });
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 2 });
    expect(r.reconcileBlockReason()).toBeNull();
  });

  test("an empty-row cursor-only delta moves the cursor and preserves every row node", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "v0"), row(1, "v1")], []);
    const n0 = viewportEl.children[0];
    const n1 = viewportEl.children[1];
    const cursor = viewportEl.children.find((child: FakeEl) => child.className === "cell-cursor") as FakeEl;

    expect(r.applyDeltaFrame({
      ...deltaFrame(80, 2, [], [], 2),
      cursorRow: 1,
      cursorCol: 3,
    })).toBe(true);

    expect(viewportEl.children[0]).toBe(n0);
    expect(viewportEl.children[1]).toBe(n1);
    expect(cursor.style.top).toBe("1lh");
    expect(cursor.style.left).toBe("3ch");
    expect(cursor.dataset).toMatchObject({ row: "1", column: "3", visible: "true" });
    expect(r.presentationSnapshot().cursor).toEqual({
      canonical: { visible: true, row: 1, column: 3 },
      dom: { visible: true, row: 1, column: 3, connected: true },
    });
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 2 });
  });

  test("cursor-only pending state resumes cleanly without replacing row nodes", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "v0"), row(1, "v1")], []);
    const n0 = viewportEl.children[0];
    const n1 = viewportEl.children[1];

    r.enterReading("wheel");
    expect(r.applyDeltaFrame({
      ...deltaFrame(80, 2, [], [], 2),
      cursorRow: 1,
      cursorCol: 4,
    })).toBe(true);
    expect(r.presentationSnapshot().cursor).toEqual({
      canonical: { visible: true, row: 1, column: 4 },
      dom: { visible: true, row: 0, column: 0, connected: true },
    });

    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: false });
    expect(viewportEl.children[0]).toBe(n0);
    expect(viewportEl.children[1]).toBe(n1);
    expect(r.presentationSnapshot().cursor).toEqual({
      canonical: { visible: true, row: 1, column: 4 },
      dom: { visible: true, row: 1, column: 4, connected: true },
    });
  });

  test("a one-row delta replaces ONLY that row's node, positionally", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "v0"), row(1, "v1")], []);
    const n0 = viewportEl.children[0];
    const n1 = viewportEl.children[1];
    r.apply(deltaFrame(80, 2, [row(1, "v1-changed")], [], 2));
    expect(viewportEl.children[0]).toBe(n0); // untouched → zero DOM writes
    expect(viewportEl.children[1]).not.toBe(n1); // replaced in place
    expect(viewportEl.children[1].children[0].textContent).toBe("v1-changed");
  });

  test("a sparse delta never reads or hashes an untouched held row", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "stable"), row(1, "old")], []);
    const stable = r.currentFrame!.viewportRows[0]!;
    Object.defineProperty(stable, "spans", {
      configurable: true,
      get: () => { throw new Error("untouched row was inspected"); },
    });

    expect(r.applyDeltaFrame(deltaFrame(80, 2, [row(1, "new")], [], 2))).toBe(true);
    expect(vpEl(c).children[1].children[0].textContent).toBe("new");
  });

  test("a viewport-only full frame rebuild prunes surplus viewport rows", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "v0"), row(1, "v1"), row(2, "v2")], []);
    const n0 = viewportEl.children[0];
    expect(viewportEl.children.length).toBe(5);
    seedHeldHistory(r, 80, [row(0, "v0")], []);
    expect(viewportEl.children.length).toBe(3);
    expect(viewportEl.children[0]).not.toBe(n0);
  });

  test("a scrolling delta REUSES shifted row nodes; only the new tail renders", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "A"), row(1, "B"), row(2, "C")], []);
    const nB = viewportEl.children[1];
    const nC = viewportEl.children[2];
    // One line scrolled out: A moved to scrollback. Only newly exposed D is
    // carried as a dirty row; B/C transfer through the canonical model + DOM.
    expect(r.applyDeltaFrame({
      ...deltaFrame(80, 3, [row(2, "D")], [row(0, "A")], 2),
      scrollbackTotal: 1,
    })).toBe(true);
    expect(viewportEl.children[0]).toBe(nB); // shifted up, node reused
    expect(viewportEl.children[1]).toBe(nC); // shifted up, node reused
    expect(viewportEl.children[2]).not.toBe(nC); // the only newly rendered row
    expect(viewportEl.children[2].children[0].textContent).toBe("D");
    expect(r.gridText()).toBe("B\nC\nD");
  });

  test("a scrolling delta without every exposed tail row requests repair", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "A"), row(1, "B"), row(2, "C")], []);
    const before = r.gridText();
    expect(r.applyDeltaFrame({
      ...deltaFrame(80, 3, [], [row(0, "A")], 2),
      scrollbackTotal: 1,
    })).toBe(false);
    expect(r.gridText()).toBe(before);
  });
});

// ── viewport-only full frames + explicit backfill splice ──────────────────
describe("CellGridRenderer DOM — viewport-only frames + backfill", () => {

  test("viewport-only full reserves depth; explicit pages fill the seam", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = sbEl(c);
    r.apply({
      ...fullFrame(80, [row(0, "v")]),
      scrollbackTotal: 4,
      sbBase: 4,
    });
    expect(sbRows(scrollbackEl)).toHaveLength(0);
    r.prependScrollback([row(2, "h2"), row(3, "h3")]);
    const a0 = r.backfillAnchor()!;
    expect(a0.sbBase).toBe(2);
    expect(a0.gridEpoch).toBe("test-grid:0");
    const newestPage = sbRows(scrollbackEl);
    r.prependScrollback([row(0, "h0"), row(1, "h1")]);
    const all = sbRows(scrollbackEl);
    expect(all.map((n) => n.children[0].textContent)).toEqual(["h0", "h1", "h2", "h3"]);
    expect(all[2]).toBe(newestPage[0]);
    expect(all[3]).toBe(newestPage[1]);
    expect(r.backfillAnchor()!.sbBase).toBe(0);
    r.prependScrollback([row(0, "stale")]);
    expect(sbRows(scrollbackEl)).toHaveLength(4);
  });

  test("a delta after explicit backfill keeps appending at the same epoch", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const scrollbackEl = sbEl(c);
    r.apply({
      ...fullFrame(80, [row(0, "v")]),
      scrollbackTotal: 2,
      sbBase: 2,
    });
    r.prependScrollback([row(1, "h1")]);
    r.apply({ ...deltaFrame(80, 1, [], [row(2, "h2")], 3), scrollbackTotal: 3 });
    expect(sbRows(scrollbackEl)).toHaveLength(2);
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
    expect(r.heldFrameSeq()).toBe(10);
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
    seedHeldHistory(r, 80, [row(0, "v")], seq(300).map((i) => row(300 + i, `h${i}`)), 600);
    expect((scrollbackEl.children[0] as FakeEl).style["overflow-anchor"]).toBeUndefined();
    expect((scrollbackEl.children[1] as FakeEl).style["overflow-anchor"]).toBe("none");

    r.prependScrollback([row(299, "backfill")]);
    expect((scrollbackEl.children[2] as FakeEl).style["overflow-anchor"]).toBeUndefined();

    r.apply(appDelta([row(600, "stream")], 601, 2));
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
    r.apply(appDelta(append, 2000 + BLOCK, 99));
    expect(r.currentFrame!.sbBase).toBe(baseBefore);
    expect(r.currentFrame!.scrollbackRows.length).toBe(2000);
    expect(r.heldFrameSeq()).toBe(99);
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
    seedHeldHistory(r, 80, [row(0, "v")], seqN(7).map((i) => row(i, `s${i}`)));
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

// ── explicit reader intent + persistent live tail ─────────────────────────
// Geometry is an effect, not intent. Live panes follow every accepted frame;
// only explicit native/find/selection actions freeze canonical state from DOM.
describe("CellGridRenderer DOM — reader intent and live tail", () => {
  const nRows = (n: number, from = 0) =>
    Array.from({ length: n }, (_, i) => row(from + i, `s${from + i}`));
  const appDelta = (append: CellRow[], total: number, seq: number): CellGridFrame =>
    ({ ...deltaFrame(80, 1, [row(0, "v")], append, seq), scrollbackTotal: total });

  test("live output follows the tail and its owned scroll event stays live", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();

    r.apply(appDelta([row(400, "new")], 401, 3));

    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
    expect(r.readerIntent).toBe("live");
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("live");
  });

  test("a coalesced pin retargets once, then the next native scroll reads", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    const bottom = Math.max(0, c.scrollHeight - c.clientHeight);
    c.scrollTop = bottom;
    r.handleScroll(); // observe the seed render before starting this epoch

    r.setSelectionHold(true);
    c.scrollTop = bottom - 1;
    expect(r.apply({ ...appDelta([], 400, 3), cursorCol: 1 })).toBe(true);
    c.resetScrollTopWrites();
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: false });
    expect(c.scrollTopWrites).toBe(1);

    // Before the event is delivered, layout clamps the scroll position and a
    // second live pin is already unchanged at that final coalesced value.
    c.clientHeight += ROW_PX;
    const finalOwnedTop = c.scrollHeight - c.clientHeight;
    c.scrollTop = finalOwnedTop;
    c.resetScrollTopWrites();
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: false, anchorChanged: false });
    expect(c.scrollTopWrites).toBe(0);
    expect(r.canonicalEpochSeq()).toEqual(r.reconciledEpochSeq());
    expect(r.readerIntent).toBe("live");

    c.clientHeight -= 1; // the one delivered event now observes off-bottom geometry
    expect(r.atBottom()).toBe(false);
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("live");

    c.scrollTop = finalOwnedTop - ROW_PX;
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("native_scroll");
  });

  test("selection release re-pins before its owned event, then the next wheel reads", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    const bottom = Math.max(0, c.scrollHeight - c.clientHeight);
    c.scrollTop = bottom;
    r.handleScroll(); // consume the seed render's owned event

    // Actual Chromium order for an admitted key with a retained native range:
    // prepare/reconcile → Selection.removeAllRanges → scrollTop=0 scroll →
    // selectionchange → live re-pin scroll.
    r.setSelectionHold(true);
    expect(r.apply({ ...appDelta([], 400, 3), cursorCol: 1 })).toBe(true);
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: false });
    r.beginLiveSelectionRelease();
    c.scrollTop = 0;
    c.resetScrollTopWrites();

    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(c.scrollTop).toBe(bottom);
    expect(c.scrollTopWrites).toBe(1);
    expect(r.readerIntent).toBe("live");
    expect(r.readerReason).toBeNull();
    expect(r.holdMask).toBe(0);
    expect(r.canonicalEpochSeq()).toEqual(r.reconciledEpochSeq());

    r.finishLiveSelectionRelease(); // asynchronous selectionchange
    c.resetScrollTopWrites();
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(c.scrollTopWrites).toBe(0);
    expect(r.readerIntent).toBe("live");

    // The lifecycle bracket and its repin ownership are both gone. The very
    // next genuine gesture must be visible and keep its explicit reason.
    r.enterReading("wheel");
    c.scrollTop = bottom - ROW_PX;
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("wheel");
  });

  test("unchanged and fully clamped pins leave no stale scroll ownership", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    const bottom = Math.max(0, c.scrollHeight - c.clientHeight);
    c.scrollTop = bottom;
    r.handleScroll();
    c.scrollTop = bottom - ROW_PX;
    r.handleScroll(); // mismatching native position ends the seed write's epoch

    const clampedTop = c.scrollTop;
    c.nextScrollTopWriteResult = clampedTop;
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("live");
    expect(r.atBottom()).toBe(false);
    r.handleScroll();
    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("native_scroll");

    c.scrollTop = bottom;
    r.handleScroll();
    expect(r.readerIntent).toBe("live");
    c.clientHeight -= 1;
    expect(r.atBottom()).toBe(false);
    r.handleScroll();
    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("native_scroll");
  });

  test("transient off-bottom geometry does not freeze a live frame", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = c.scrollHeight - c.clientHeight - 1;
    c.resetScrollTopWrites();

    r.apply(appDelta([row(400, "new")], 401, 3));

    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
    expect(r.readerIntent).toBe("live");
  });

  test("passive full and delta output freeze behind explicit native reading", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const viewportEl = vpEl(c);
    seedHeldHistory(r, 80, [row(0, "old-v")], nRows(400));
    c.scrollTop = PAD_TOP + 50 * ROW_PX;
    expect(r.handleScroll()).toEqual({ reconciled: false, anchorChanged: false });
    const heldRow = viewportEl.children[0];
    const heldHeight = c.scrollHeight;
    c.resetScrollTopWrites();

    expect(r.apply({
      ...altFullFrame(80, [row(0, "TUI")], []),
      cursorKeysApp: true,
      bracketedPaste: true,
      scrollbackTotal: 410,
      sbBase: 410,
      seq: 3,
    })).toBe(true);
    expect(r.apply({
      ...altDeltaFrame(80, 1, [row(0, "latest")], 4),
      cursorKeysApp: true,
      bracketedPaste: true,
      scrollbackAppend: [row(410, "live-410")],
      scrollbackTotal: 411,
    })).toBe(true);

    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("native_scroll");
    expect(r.currentFrame!.gridEpoch).toBe("test-grid:0");
    expect(r.heldFrameSeq()).toBe(4);
    expect(viewportEl.children[0]).toBe(heldRow);
    expect(c.scrollHeight).toBe(heldHeight);
    expect(c.scrollTopWrites).toBe(0);
    expect(r.presentationSnapshot().mode).toEqual({
      canonical: {
        alt_screen: true,
        cursor_keys_app: true,
        bracketed_paste: true,
      },
      reconciled: {
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
      },
    });
  });

  test("a genuine return to literal bottom reconciles pending canonical state", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old-v")], nRows(400));
    c.scrollTop = PAD_TOP + 50 * ROW_PX;
    r.handleScroll();
    r.apply({
      ...appDelta([row(400, "new")], 401, 3),
      viewportRows: [row(0, "latest-v")],
    });
    expect(r.currentFrame!.seq).toBe(2);

    c.scrollTop = c.scrollHeight - c.clientHeight;
    c.resetScrollTopWrites();
    expect(r.handleScroll()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.currentFrame!.seq).toBe(3);
    expect(spansText((r.currentFrame!.viewportRows[0]!).spans)).toBe("latest-v");
    expect(r.readerIntent).toBe("live");
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
  });

  test("find and native reader actions stay frozen until explicit live preparation", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));

    r.scrollToScrollbackRow(50);
    expect(r.readerIntent).toBe("reading");
    expect(r.readerReason).toBe("find");
    r.handleScroll(); // consumes the find-owned write
    expect(r.readerReason).toBe("find");
    r.apply(appDelta([row(400, "new")], 401, 3));
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.readerIntent).toBe("live");

    c.scrollTop = c.scrollHeight - c.clientHeight - ROW_PX;
    r.handleScroll();
    expect(r.readerReason).toBe("native_scroll");
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.readerIntent).toBe("live");
    expect(r.atBottom()).toBe(true);
  });

  test("a non-bottom backfill prepend performs no application scroll write", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const total = 1000, held = 300;
    seedHeldHistory(r, 80, [row(0, "v")], nRows(held, total - held), total);
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

  test("live intent persists through box changes before ResizeObserver runs", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();
    c.clientHeight = 400;

    for (let k = 0; k < 3; k++) {
      r.apply(appDelta(nRows(50, 400 + 50 * k), 450 + 50 * k, 3 + k));
    }

    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(r.atBottom()).toBe(true);
    expect(r.readerIntent).toBe("live");
    expect(c.scrollTopWrites).toBe(3);
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
    seedHeldHistory(r, 80, [row(0, "v")], nRows(250, 500), 750);

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
    seedHeldHistory(r, 80, [row(0, "v")], nRows(250, 500), 750);
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
    seedHeldHistory(r, 80, [row(0, "v")], nRows(held, 500), 500 + held);
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


  test("a same-epoch streaming repair freezes explicit reading until atomic resume", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old-v")], nRows(250, 500), 750);
    const reconciledBefore = r.reconciledEpochSeq();
    c.scrollTop = PAD_TOP + 600 * ROW_PX;
    r.handleScroll();
    const readerBefore = rowAtReader(c);
    const heightBefore = c.scrollHeight;
    c.resetScrollTopWrites();

    r.apply({ ...fullFrame(80, [row(0, "repair-v")], 760), seq: 3 });
    r.apply({
      ...deltaFrame(80, 1, [row(0, "latest-v")], [row(760, "live-760")], 4),
      scrollbackTotal: 761,
    });

    expect(r.currentFrame!.scrollbackTotal).toBe(750);
    expect(spansText((r.currentFrame!.viewportRows[0]!).spans)).toBe("old-v");
    expect(r.heldFrameSeq()).toBe(4);
    expect(r.canonicalEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 4 });
    expect(r.reconciledEpochSeq()).toEqual(reconciledBefore);
    expect(r.reconcileBlockReason()).toBe("reader_pending_frame");
    expect(c.scrollHeight).toBe(heightBefore);
    expect(c.scrollTopWrites).toBe(0);
    expect(rowAtReader(c)).toBe(readerBefore);

    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.currentFrame!.scrollbackTotal).toBe(761);
    expect(r.currentFrame!.sbBase).toBe(760);
    expect(r.currentFrame!.scrollbackRows[0]!.index).toBe(760);
    expect(spansText((r.currentFrame!.viewportRows[0]!).spans)).toBe("latest-v");
    expect(c.scrollTopWrites).toBe(1);
    expect(r.atBottom()).toBe(true);
    expect(r.reconciledEpochSeq()).toEqual({ grid_epoch: "test-grid:0", seq: 4 });
    expect(r.reconcileBlockReason()).toBeNull();
  });

  test("releasing selection alone preserves reading and pending canonical state", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old-v")], nRows(250, 500), 750);
    c.scrollTop = PAD_TOP + 600 * ROW_PX;
    r.setSelectionHold(true);
    r.apply({ ...fullFrame(80, [row(0, "latest-v")], 760), seq: 3 });

    expect(r.setSelectionHold(false)).toEqual({ reconciled: false, anchorChanged: false });
    expect(r.currentFrame!.seq).toBe(2);
    expect(r.readerIntent).toBe("reading");
    c.resetScrollTopWrites();
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.currentFrame!.seq).toBe(3);
    expect(spansText((r.currentFrame!.viewportRows[0]!).spans)).toBe("latest-v");
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
  });
});

// ── synchronous box reconciliation ───────────────────────────────────────
// ResizeObserver can run after layout and after another cell frame. The old
// literal-bottom sample repairs that interleave immediately, while explicit
// off-bottom reading remains untouched.
describe("CellGridRenderer DOM — box resize + unreachable window", () => {
  const spPx = (c: FakeEl): number =>
    parseFloat(String((c.children.find((x: FakeEl) => x.className === "cell-sb-spacer") as FakeEl).style.height));
  const nRows = (n: number, from = 0) =>
    Array.from({ length: n }, (_, i) => row(from + i, `b${from + i}`));

  test("a live old-bottom anchor follows a box shrink with exactly one pin", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();

    c.clientHeight = 400; // divider drag / window resize under a parked pane
    r.noteBoxResize();

    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
    expect(r.atBottom()).toBe(true);

    r.noteBoxResize(); // same height again — observer re-tick is a no-op
    expect(c.scrollTopWrites).toBe(1);
  });

  test("old-bottom resize reconciles a frame that arrived after layout without scroll", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "old")], nRows(400));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    r.handleScroll(); // consume the seed pin's owned event

    c.clientHeight = 400; // layout happens before ResizeObserver
    r.handleScroll(); // model the geometry event that briefly looks native
    expect(r.readerIntent).toBe("reading");
    expect(r.apply({
      ...fullFrame(80, [row(0, "after-layout")], 410),
      scrollbackTotal: 410,
      sbBase: 410,
      seq: 3,
    })).toBe(true);
    expect(r.currentFrame!.seq).toBe(2);
    c.resetScrollTopWrites();

    expect(r.noteBoxResize()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.currentFrame!.seq).toBe(3);
    expect(spansText((r.currentFrame!.viewportRows[0]!).spans)).toBe("after-layout");
    expect(r.readerIntent).toBe("live");
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
  });

  test("off-bottom reader is untouched by a box shrink", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = c.scrollHeight - c.clientHeight - 2; // >1px above the old bottom
    const before = c.scrollTop;
    c.resetScrollTopWrites();

    c.clientHeight = 400;
    r.noteBoxResize();

    expect(c.scrollTop).toBe(before);
    expect(c.scrollTopWrites).toBe(0);
  });

  test("at-bottom reader follows a box grow onto the new bottom", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(400));
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    c.resetScrollTopWrites();

    // Grow: the scroll maximum DROPS below the held scrollTop. A real browser
    // clamps scrollTop onto the new bottom; the fake has no clamp, so the pin
    // is what lands the reader there — max(prev, h) reads the over-max offset
    // as at-bottom either way.
    c.clientHeight = 700;
    r.noteBoxResize();

    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
    expect(r.atBottom()).toBe(true);
  });

  test("an epoch-changing full frame resumes atomically from explicit reading", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(250, 500), 750);
    c.scrollTop = PAD_TOP + 600 * ROW_PX;
    r.handleScroll();
    const before = c.scrollTop;
    c.resetScrollTopWrites();

    r.apply({
      ...fullFrame(80, [row(0, "v")], 5000),
      gridEpoch: "test-grid:1",
      scrollbackTotal: 5000,
      sbBase: 5000,
      seq: 3,
    });

    expect(c.scrollTop).toBe(before);
    expect(c.scrollTopWrites).toBe(0);
    expect(r.currentFrame!.gridEpoch).toBe("test-grid:0");
    expect(r.heldFrameSeq()).toBe(3);
    expect(r.prepareLiveInteraction()).toEqual({ reconciled: true, anchorChanged: true });
    expect(r.currentFrame!.gridEpoch).toBe("test-grid:1");
    expect(c.scrollTop).toBe(c.scrollHeight - c.clientHeight);
    expect(c.scrollTopWrites).toBe(1);
  });

  test("renderFull reserves the incoming spacer BEFORE wiping painted history", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(250, 500), 750);
    c.scrollTop = Math.max(0, c.scrollHeight - c.clientHeight);
    const preScrollTop = c.scrollTop;

    // Spy the wipe: at the instant .cell-scrollback is cleared, the spacer must
    // already hold the INCOMING frame's reserve — the scroll maximum never dips
    // below the reader's offset, so the browser never clamps them into blank
    // space (whose scroll event would start a top-down backfill drain).
    const sb = sbEl(c);
    const orig = sb.replaceChildren.bind(sb);
    let spacerAtWipe = -1;
    let heightAtWipe = -1;
    sb.replaceChildren = (...kids: unknown[]) => {
      orig(...kids);
      spacerAtWipe = spPx(c);
      heightAtWipe = c.scrollHeight;
    };

    // Width change → slow path → renderFull replaceChildren.
    r.apply({ ...fullFrame(100, [row(0, "v")], 6000), gridEpoch: "test-grid:1", seq: 3 });

    expect(spacerAtWipe).toBe(6000 * ROW_PX);
    expect(heightAtWipe).toBeGreaterThanOrEqual(preScrollTop);
  });
});

// ── the browser half of the layered history snapshot ──────────────────────
// The worker's diagnostic snapshot reports the CORE's live range and the RING's
// byte bounds; neither says what the browser actually holds, so "the browser is
// missing history" and "the worker never had it" were the same observation. The
// probe carries the held range in the SAME payload as the worker's two, read off
// the very anchor the paging controller addresses history with.
describe("terminalBrowserStreamSnapshot — the range this document holds", () => {
  const nRows = (n: number, from = 0) =>
    Array.from({ length: n }, (_, i) => row(from + i, `r${from + i}`));

  test("reports the held range, not the frame's total, and follows a prepend", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    // 250 painted rows of a 750-row history: the shape a full frame always has.
    seedHeldHistory(r, 80, [row(0, "v")], nRows(250, 500), 750);
    const release = registerRenderer("probe-session", r);

    const held = terminalBrowserStreamSnapshot("probe-session").history;
    expect(held).toEqual({
      grid_epoch: "test-grid:0",
      sb_base: 500,
      total: 750,
      cols: 80,
      rows_held: 250,
      floor: null,
    });

    // A backfill page moves the held range down; the probe must move with it or a
    // stale sb_base reads as history the browser does not actually have.
    r.prependScrollback(nRows(100, 400));
    const after = terminalBrowserStreamSnapshot("probe-session").history;
    expect(after.sb_base).toBe(400);
    expect(after.rows_held).toBe(350);
    expect(after.total).toBe(750);
    release();
  });

  test("an unregistered session reports explicit nulls rather than zeros", () => {
    // A pane that never mounted holds NO range. Zeros would read as "holds all of
    // history from row 0", which is the opposite of the truth.
    expect(terminalBrowserStreamSnapshot("never-mounted").history).toEqual({
      grid_epoch: null,
      sb_base: null,
      total: null,
      cols: null,
      rows_held: 0,
      floor: null,
    });
  });
});
