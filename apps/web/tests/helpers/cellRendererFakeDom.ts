// Fake DOM + frame builders shared by the CellGridRenderer DOM tripwire suite
// (cellRenderer.*.dom.test.ts) and the terminalBrowserStreamSnapshot test.
//
// No jsdom (by design, per cellRenderer.test.ts). A ~40-line fake DOM covers
// exactly what CellGridRenderer touches — node identity is all we assert.

import { DEFAULT_COLOR, type CellGridFrame, type CellRow } from "@roost/shared/cell";
import type { CellGridRenderer } from "../../src/lib/cellRenderer.ts";

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
export const PAD_TOP = 12; // .wterm padding-top (styles/sidebar.css)
export const ROW_PX = 16;  // one .cell-row line box
export const CELL_PX = 8;  // one column advance (1ch in the display font)
export const PANE_PX = 1000; // .wterm client width — wider than the grid, so rows letterbox

export class FakeEl {
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
  // Client-space box, derived from the same painted-rows layout model as
  // scrollHeight/offsetTop. That is what lets a test tell the SCROLL
  // CONTAINER's top (history, then the spacer) from .cell-viewport's top (row 1
  // of the live grid) — the distinction pointer hit-testing got wrong.
  // Widths mirror the real CSS: .cell-scrollback/.cell-viewport are
  // calc(var(--cell-cols) * 1ch); the pane itself is wider (letterbox).
  // A .cell-row is exactly one line box — what rowHeight()'s probe measures.
  getBoundingClientRect() {
    const height = this.className === "cell-row" ? ROW_PX : this.paintedRows * ROW_PX;
    const width = this.className === "cell-viewport" || this.className === "cell-scrollback"
      ? this.gridCols() * CELL_PX
      : PANE_PX;
    const top = this.clientTop();
    return { height, width, top, left: 0, bottom: top + height, right: width };
  }
  // --cell-cols, written on the scroll container by CellGridRenderer.setGridWidth.
  private gridCols(): number {
    let root: FakeEl = this;
    while (root.parentElement) root = root.parentElement;
    return parseFloat(String((root.style as Record<string, unknown>)["--cell-cols"] ?? "")) || 80;
  }
  // The container's own box starts at 0; a child starts below the painted height
  // of its preceding siblings, plus the pane's padding, minus how far the
  // scroller has been scrolled.
  private clientTop(): number {
    const p = this.parentElement;
    if (!p) return 0;
    let y = p.clientTop() + (p.parentElement ? 0 : PAD_TOP - p.scrollTop);
    for (const sib of p.children as FakeEl[]) {
      if (sib === this) break;
      y += (sib.paintedRows ?? 0) * ROW_PX;
    }
    return y;
  }
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
export function makeContainer(): FakeEl {
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
export function row(index: number, text: string): CellRow {
  return { index, spans: text ? [{ text, columns: text.length, fg: DEFAULT_COLOR, bg: DEFAULT_COLOR, flags: 0 }] : [] };
}
const BASE = {
  gridEpoch: "test-grid:0",
  cursorRow: 0, cursorCol: 0, cursorVisible: true, altScreen: false,
  cursorKeysApp: false, bracketedPaste: false,
  mouseTracking: 0, mouseSgr: false, focusEvents: false,
} as const;
export function fullFrame(cols: number, viewport: CellRow[], scrollbackTotal = 0): CellGridFrame {
  return { ...BASE, cols, rows: viewport.length, full: true,
    viewportRows: viewport, scrollbackRows: [], scrollbackAppend: [],
    scrollbackTotal, sbBase: scrollbackTotal, seq: 1 };
}
export function deltaFrame(
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
export function seedHeldHistory(
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
export function altFullFrame(cols: number, viewport: CellRow[], _history: CellRow[]): CellGridFrame {
  return { ...fullFrame(cols, viewport), gridEpoch: "test-grid:1", altScreen: true };
}
export function altDeltaFrame(cols: number, rows: number, viewport: CellRow[], seq: number): CellGridFrame {
  return { ...deltaFrame(cols, rows, viewport, [], seq), gridEpoch: "test-grid:1", altScreen: true };
}

// Scrollback rows are packed into .cell-block wrappers (SB_BLOCK content-
// visibility blocks — WIP block packing); flatten to assert ROW identity.
export const sbEl = (c: FakeEl): FakeEl => c.children.find((x: FakeEl) => x.className === "cell-scrollback") as FakeEl;
export const vpEl = (c: FakeEl): FakeEl => c.children.find((x: FakeEl) => x.className === "cell-viewport") as FakeEl;
export const sbRows = (scrollbackEl: FakeEl): FakeEl[] =>
  scrollbackEl.children.flatMap((b: FakeEl) => b.children) as FakeEl[];
