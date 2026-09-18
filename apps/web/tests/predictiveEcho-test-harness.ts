// Shared fixture for the predictive-echo unit tiers (predictiveEcho.test.ts,
// predictiveEchoAck.test.ts). There is no jsdom in this repo, so this owns a
// fake DOM just large enough for the overlay, an injectable clock standing in
// for link RTT, the input-sequence bookkeeping the engine's ack gate needs, and
// a reader for the cells the overlay actually painted.

import { PredictiveEcho } from "../src/lib/predictiveEcho.ts";
import type { PredictMode } from "../src/lib/predictPref.ts";
import type { CellGridFrame, CellSpan } from "@roost/shared/cell";

export interface PaintedCell {
  ch: string;
  className: string;
  left: string;
  top: string;
}

export type FrameRow = string | null | readonly CellSpan[];

// ── minimal fake DOM (only what the overlay touches) ──────────────────

interface FakeNode {
  className: string;
  textContent: string;
  style: Record<string, string>;
  parentNode: FakeNode | null;
  ownerDocument: FakeDocument | null;
  /** Children, in document order. Named with the overlay's reader in mind. */
  _kids: FakeNode[];
  _isFragment: boolean;
  appendChild(child: FakeNode): FakeNode;
  replaceChildren(...nodes: FakeNode[]): void;
  remove(): void;
}

interface FakeDocument {
  createElement(tag: string): FakeNode;
  createDocumentFragment(): FakeNode;
}

export function fakeEl(): FakeNode {
  const el: FakeNode = {
    className: "", textContent: "", style: {}, parentNode: null,
    ownerDocument: null, _kids: [], _isFragment: false,
    appendChild(child) { child.parentNode = el; el._kids.push(child); return child; },
    replaceChildren(...nodes) {
      el._kids = [];
      for (const node of nodes) {
        // A fragment contributes its children, not itself — the overlay builds
        // one fragment per repaint, so ignoring it would discard every span.
        if (node._isFragment) {
          for (const kid of node._kids) { kid.parentNode = el; el._kids.push(kid); }
          node._kids = [];
          continue;
        }
        node.parentNode = el;
        el._kids.push(node);
      }
    },
    remove() { el.parentNode = null; },
  };
  return el;
}

export function fakeHost(): HTMLElement {
  const doc: FakeDocument = {
    createElement() { const el = fakeEl(); el.ownerDocument = doc; return el; },
    createDocumentFragment() {
      const frag = fakeEl();
      frag.ownerDocument = doc;
      frag._isFragment = true;
      return frag;
    },
  };
  const host = fakeEl();
  host.ownerDocument = doc;
  // Structural stand-in for the viewport element: the overlay touches only the
  // members FakeNode declares, and a runtime check is meaningless here.
  return host as unknown as HTMLElement;
}

export function frame(o: {
  seq: number; cc?: number; cr?: number; rows?: FrameRow[];
  alt?: boolean; full?: boolean; cols?: number;
}): CellGridFrame {
  const rows = o.rows ?? [null];
  return {
    streamId: "test-stream:0",
    gridEpoch: "test-grid:0",
    cols: o.cols ?? 80, rows: 24,
    cursorRow: o.cr ?? 0, cursorCol: o.cc ?? 0, cursorVisible: true,
    altScreen: o.alt ?? false, cursorKeysApp: false, bracketedPaste: false, full: o.full ?? false,
    mouseTracking: 0, mouseSgr: false, focusEvents: false,
    viewportRows: rows.map((row, i) => ({ index: i, spans: rowSpans(row) })),
    scrollbackRows: [], scrollbackAppend: [], scrollbackTotal: 0, sbBase: 0,
    seq: o.seq,
    baseSeq: o.full === true ? 0 : Math.max(0, o.seq - 1),
  } as CellGridFrame;
}

export const enc = (text: string): Uint8Array => new TextEncoder().encode(text);

/** One clock object shared by every case, so both test files read the same
 *  instance after mk() rewinds it. */
export const clock = { t: 0 };

/** The engine's ack gate is driven by the input-admission sequence, which the
 *  transport mints monotonically per browser session. */
let nextInputSeq = 0n;

export function mk(mode: PredictMode = "adaptive"): PredictiveEcho {
  return mkWithHost(mode).pe;
}

/** mk() plus the host, for the cases that assert on painted DOM. `anchor` is
 *  the frame that seeds grid state; `null` leaves the engine unanchored. */
export function mkWithHost(
  mode: PredictMode = "adaptive",
  opts: {
    onCursor?: (col: number | null) => void;
    anchor?: CellGridFrame | null;
  } = {},
): { pe: PredictiveEcho; host: HTMLElement } {
  clock.t = 0;
  nextInputSeq = 0n;
  const host = fakeHost();
  const pe = new PredictiveEcho(host, {
    mode: () => mode,
    now: () => clock.t,
    onCursor: opts.onCursor,
    // No live timer may outlive a case; expiry is driven by _expirePredictions.
    schedule: () => () => {},
  });
  const anchor = opts.anchor === undefined
    ? frame({ seq: 1, full: true, cc: 0 })
    : opts.anchor;
  if (anchor !== null) pe.onFrame(anchor);
  return { pe, host };
}

/** Type bytes AND ack their PTY write — what every ordinary keystroke has done
 *  by the time its echo can arrive. */
export function type(pe: PredictiveEcho, input: string | Uint8Array): bigint {
  const seq = typeUnacked(pe, input);
  pe.noteInputWritten(seq);
  return seq;
}

/** Type bytes whose write the worker has NOT acknowledged yet. */
export function typeUnacked(pe: PredictiveEcho, input: string | Uint8Array): bigint {
  nextInputSeq += 1n;
  pe.predict(typeof input === "string" ? enc(input) : input, nextInputSeq);
  return nextInputSeq;
}

export function paintedCells(host: HTMLElement): PaintedCell[] {
  const overlay = (host as unknown as FakeNode)._kids[0];
  if (!overlay) return [];
  return overlay._kids.map((kid) => ({
    ch: kid.textContent,
    className: kid.className,
    left: kid.style.left ?? "",
    top: kid.style.top ?? "",
  }));
}

function rowSpans(row: FrameRow): readonly CellSpan[] {
  if (row === null) return [];
  if (typeof row !== "string") return row;
  return [{ text: row, columns: row.length, fg: 256, bg: 256, flags: 0 }];
}
