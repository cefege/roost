// Predictive-echo state machine (lib/predictiveEcho.ts) — the epoch/
// confidence logic. No jsdom in this repo, so a tiny fake DOM + an injectable
// clock (to simulate link RTT) drive the SM; assertions read the _debug() seam,
// not the DOM. Covers: SRTT gate (no-op on fast link), the two-epoch confidence
// gate (first keystroke of a burst hidden until confirmed), hard-reset on a
// shown wrong guess, and alt-screen suppression.

import { describe, test, expect, beforeEach } from "bun:test";
import { PredictiveEcho } from "../src/lib/predictiveEcho.ts";
import type { CellGridFrame } from "@roost/shared/cell";

// bun test has no DOM/localStorage — stub the kill-switch storage.
const _ls: Record<string, string> = {};
(globalThis as unknown as { localStorage: Storage }).localStorage = {
  getItem: (k: string) => _ls[k] ?? null,
  setItem: (k: string, v: string) => { _ls[k] = v; },
  removeItem: (k: string) => { delete _ls[k]; },
  clear: () => { for (const k of Object.keys(_ls)) delete _ls[k]; },
  key: () => null, length: 0,
} as Storage;

// ── minimal fake DOM (only what the overlay touches) ──────────────────
function fakeEl(): any {
  const el: any = {
    className: "", textContent: "", style: {}, parentNode: null, _kids: [],
    ownerDocument: null,
    appendChild(c: any) { c.parentNode = el; el._kids.push(c); return c; },
    replaceChildren() { el._kids = []; },
    remove() { el.parentNode = null; },
  };
  return el;
}
function fakeHost(): HTMLElement {
  const doc: any = {
    createElement() { const e = fakeEl(); e.ownerDocument = doc; return e; },
    createDocumentFragment() { return { appendChild() {} }; },
  };
  const host = fakeEl(); host.ownerDocument = doc;
  return host as unknown as HTMLElement;
}

function frame(o: { seq: number; cc?: number; cr?: number; rows?: (string | null)[]; alt?: boolean; full?: boolean; cols?: number }): CellGridFrame {
  const rows = o.rows ?? [null];
  return {
    cols: o.cols ?? 80, rows: 24,
    cursorRow: o.cr ?? 0, cursorCol: o.cc ?? 0, cursorVisible: true,
    altScreen: o.alt ?? false, cursorKeysApp: false, bracketedPaste: false, full: o.full ?? false,
    viewportRows: rows.map((t, i) => ({ index: i, spans: t ? [{ text: t, fg: 256, bg: 256, flags: 0 }] : [] })),
    scrollbackRows: [], scrollbackAppend: [], scrollbackTotal: 0, sbBase: 0,
    seq: o.seq,
  } as CellGridFrame;
}

const enc = (s: string) => new TextEncoder().encode(s);

let clock = { t: 0 };
function mk(): PredictiveEcho {
  clock = { t: 0 };
  const pe = new PredictiveEcho(fakeHost(), { now: () => clock.t });
  pe.onFrame(frame({ seq: 1, full: true, cc: 0 })); // initial full frame → anchor
  return pe;
}

beforeEach(() => { try { localStorage.removeItem("roostPredict"); } catch { /* noop */ } });

describe("predictive echo SM", () => {
  test("slow link: first keystroke hidden, confirmed → next keystroke shown", () => {
    const pe = mk();
    clock.t = 0;
    pe.predict(enc("a"));
    expect(pe._debug().total).toBe(1);
    expect(pe._debug().visible).toBe(0); // tentative (epoch unconfirmed) AND srtt unmeasured

    clock.t = 200; // 200ms RTT → slow link
    pe.onFrame(frame({ seq: 2, cc: 1, rows: ["a"] })); // server echoed "a"
    const d = pe._debug();
    expect(d.confirmedEpoch).toBe(1);
    expect(d.srtt).toBeGreaterThan(100);

    clock.t = 210;
    pe.predict(enc("b")); // same epoch, now confirmed → visible on the slow link
    expect(pe._debug().visible).toBe(1);
  });

  test("fast link: predictions made but never shown (SRTT gate)", () => {
    const pe = mk();
    pe.predict(enc("a"));
    clock.t = 5; // 5ms RTT → fast link
    pe.onFrame(frame({ seq: 2, cc: 1, rows: ["a"] }));
    clock.t = 6;
    pe.predict(enc("b"));
    expect(pe._debug().total).toBe(1);
    expect(pe._debug().visible).toBe(0); // srtt/2 ≈ 2.5ms ≤ 30 → no-op
  });

  test("shown wrong guess → hard reset (fall back to authoritative)", () => {
    const pe = mk();
    pe.predict(enc("a"));
    clock.t = 200; pe.onFrame(frame({ seq: 2, cc: 1, rows: ["a"] })); // confirm epoch
    clock.t = 210; pe.predict(enc("b"));
    expect(pe._debug().visible).toBe(1); // "b" is shown
    // Next frame contradicts the SHOWN "b" (col 1 is "x") → nuke all predictions.
    clock.t = 410; pe.onFrame(frame({ seq: 3, cc: 1, rows: ["ax"] }));
    expect(pe._debug().total).toBe(0);
  });

  test("alt-screen suppresses + clears predictions", () => {
    const pe = mk();
    pe.predict(enc("a"));
    expect(pe._debug().total).toBe(1);
    pe.onFrame(frame({ seq: 2, alt: true })); // entered a TUI
    expect(pe._debug().total).toBe(0);
    pe.predict(enc("xyz")); // no-op while alt
    expect(pe._debug().total).toBe(0);
  });

  test("ESC / control byte refuses (bumps epoch → stays hidden)", () => {
    const pe = mk();
    pe.predict(new Uint8Array([0x1b])); // ESC → becomeTentative, no prediction
    expect(pe._debug().total).toBe(0);
    expect(pe._debug().predictionEpoch).toBe(2);
    pe.predict(enc("a")); // epoch 2, confirmedEpoch 0 → tentative → hidden
    expect(pe._debug().visible).toBe(0);
  });

  test("kill switch (localStorage.roostPredict=0) disables", () => {
    localStorage.setItem("roostPredict", "0");
    const pe = mk();
    pe.predict(enc("a"));
    expect(pe._debug().total).toBe(0);
  });
});

// display_preference: Adaptive/Always/Never/
// Experimental. Roost maps roostPredict → mode, with '0'/'force' back-compat.
describe("display_preference modes", () => {
  test("mode resolution + back-compat aliases", () => {
    const cases: [string | null, string][] = [
      [null, "adaptive"], ["adaptive", "adaptive"],
      ["0", "never"], ["never", "never"],
      ["force", "always"], ["always", "always"],
      ["experimental", "experimental"],
    ];
    for (const [val, expected] of cases) {
      if (val === null) localStorage.removeItem("roostPredict"); else localStorage.setItem("roostPredict", val);
      expect(mk()._debug().mode).toBe(expected);
    }
  });

  test("Experimental: first keystroke shown IMMEDIATELY (no confidence gate, no RTT)", () => {
    localStorage.setItem("roostPredict", "experimental");
    const pe = mk();
    pe.predict(enc("a")); // no prior confirm, srtt=0 → still visible (epoch == confirmedEpoch)
    const d = pe._debug();
    expect(d.mode).toBe("experimental");
    expect(d.visible).toBe(1);
  });

  test("Always: shown after the epoch confirms even on a fast link", () => {
    localStorage.setItem("roostPredict", "always");
    const pe = mk();
    pe.predict(enc("a"));
    expect(pe._debug().visible).toBe(0); // first char tentative until confirmed
    clock.t = 5; pe.onFrame(frame({ seq: 2, cc: 1, rows: ["a"] })); // confirm (fast 5ms)
    clock.t = 6; pe.predict(enc("b"));
    expect(pe._debug().visible).toBe(1); // always-mode shows despite srtt≈5ms
  });

  test("right/left arrow predicts a cursor move (CSI C/D)", () => {
    localStorage.setItem("roostPredict", "always");
    const pe = mk(); // initial frame cursor at col 0
    pe.predict(new Uint8Array([0x1b, 0x5b, 0x43])); // ESC[C — right
    expect(pe._debug().predCursorCol).toBe(1);
    pe.predict(new Uint8Array([0x1b, 0x5b, 0x43])); // right again
    expect(pe._debug().predCursorCol).toBe(2);
    pe.predict(new Uint8Array([0x1b, 0x5b, 0x44])); // ESC[D — left
    expect(pe._debug().predCursorCol).toBe(1);
    expect(pe._debug().total).toBe(0); // arrows move the caret, predict no glyph
  });

  test("predicted cursor (onCursor) leads the echoed chars when shown", () => {
    localStorage.setItem("roostPredict", "experimental"); // shown immediately
    const cursorCalls: (number | null)[] = [];
    const pe = new PredictiveEcho(fakeHost(), { now: () => clock.t, onCursor: (c) => cursorCalls.push(c) });
    pe.onFrame(frame({ seq: 1, full: true, cc: 0 }));
    pe.predict(enc("ab")); // a@0, b@1 → caret should lead to col 2
    expect(cursorCalls[cursorCalls.length - 1]).toBe(2);
  });

  test("Experimental wrong guess resets only that cell (no hard reset)", () => {
    localStorage.setItem("roostPredict", "experimental");
    const pe = mk(); // initial frame seq=1
    pe.predict(enc("ab")); // a@0, b@1 — both shown immediately
    expect(pe._debug().visible).toBe(2);
    // Frame echoes "a" correctly but col1 is "x" (b was wrong).
    clock.t = 50; pe.onFrame(frame({ seq: 2, cc: 1, rows: ["ax"] }));
    // a confirmed+retired, b dropped (its own cell) — engine still in experimental, no throw.
    expect(pe._debug().mode).toBe("experimental");
    expect(pe._debug().total).toBe(0);
  });
});

// Step 3/4/5 hardening: full-frame reconciliation, resize wipe, SRTT
// hysteresis through the 20–30 ms dead-band, and the paste guard. All use the
// existing harness (frame(), mk(), the shared clock.t, the _debug() seam).
describe("prediction-engine hardening", () => {
  test("non-resize full frame reconciles instead of wiping", () => {
    const pe = mk();                  // seq 1 full → anchors cols 80, viewportRows 1
    pe.predict(enc("a"));             // a@0, bornSeq 1
    expect(pe._debug().total).toBe(1);
    clock.t = 200;
    // SAME dimensions, full frame → RECONCILE (cull), NOT wipe.
    pe.onFrame(frame({ seq: 2, full: true, cc: 1, rows: ["a"] }));
    const d = pe._debug();
    expect(d.total).toBe(0);          // confirmed + retired, not wiped-unjudged
    expect(d.srtt).toBeGreaterThan(0);// judged → SRTT sampled (old ||frame.full wiped → srtt stayed 0)
  });

  test("resize full frame still wipes (coords invalidated)", () => {
    const pe = mk();                  // cols 80
    pe.predict(enc("a"));             // a@0, bornSeq 1
    clock.t = 200;
    // cols changed → resize → wipe BEFORE reconcile (SRTT never sampled).
    pe.onFrame(frame({ seq: 2, full: true, cols: 100, cc: 1, rows: ["a"] }));
    const d = pe._debug();
    expect(d.total).toBe(0);          // wiped
    expect(d.srtt).toBe(0);           // never judged
  });

  test("hysteresis: shows through the 20–30 ms dead-band once armed", () => {
    const pe = mk();
    // Arm: a high-RTT confirmation (SRTT/2 > SHOW_ON arms srttTrigger).
    pe.predict(enc("a"));
    clock.t = 200;
    pe.onFrame(frame({ seq: 2, cc: 1, rows: ["a"] })); // srtt=200, half 100 > 30 → armed
    // Drive the EWMA down into the 40–60 ms band (half 20–30, the old dead-band
    // where the stateless gate returned false) with ~50 ms confirmations.
    // srtt = 50 + (200-50)*0.875^n → ≈53 after 29 steps (codes 98..126, < DEL).
    let row = "a", seq = 3;
    for (let i = 0; i < 29; i++) {
      const ch = String.fromCharCode(98 + i); // b, c, … ~
      pe.predict(enc(ch));
      clock.t += 50;
      row += ch;
      pe.onFrame(frame({ seq: seq++, cc: row.length, rows: [row] }));
    }
    const mid = pe._debug();
    expect(mid.srtt).toBeGreaterThan(40);      // genuinely in the dead-band…
    expect(mid.srtt).toBeLessThanOrEqual(60);  // …where OLD code hid everything
    // A fresh prediction in the band is still visible — armed persists through it.
    pe.predict(enc("0"));
    expect(pe._debug().visible).toBe(1);
  });

  test("paste guard: >100 bytes reset, never predicted", () => {
    const pe = mk();
    pe.predict(enc("a"));                 // a real keystroke
    expect(pe._debug().total).toBe(1);
    pe.predict(new Uint8Array(101));      // a paste → reset + drop
    expect(pe._debug().total).toBe(0);
  });
  test("delta frame (partial viewportRows) reconciles, not wiped by dirty-count drift", () => {
    // A DELTA frame's viewportRows holds only the CHANGED rows (types.ts:58-61,
    // grid-to-cells.ts:112-114), so viewportRows.length is the dirty-row COUNT,
    // not the viewport height. Resize must be detected via frame.rows (the
    // stable height), else consecutive deltas with differing dirty counts wipe
    // every prediction and SRTT never gets sampled — the exact failure Step 3
    // fixes. (The wire hands the predictor the RAW delta, not a diff-grid-
    // reconstructed full frame — see sync-dispatch._dispatchCell.)
    clock.t = 0;
    const pe = new PredictiveEcho(fakeHost(), { now: () => clock.t });
    // First FULL frame: 2 viewport rows → seeds the height tracker.
    pe.onFrame(frame({ seq: 1, full: true, cc: 0, rows: [null, null] }));
    pe.predict(enc("a"));                    // a@0, bornSeq 1, bornMs 0
    clock.t = 200;
    // DELTA: full:false, only ONE changed row (the echoed "a"). Same height
    // (frame.rows=24) but viewportRows.length=1 ≠ the full frame's 2.
    pe.onFrame(frame({ seq: 2, cc: 1, rows: ["a"] }));
    const d = pe._debug();
    expect(d.total).toBe(0);                  // reconciled + retired, NOT wiped
    expect(d.srtt).toBeGreaterThan(0);        // judged → SRTT sampled (the bug wiped → srtt stayed 0)
  });
  test("delta confirm reads the right row by .index (cursor off row 0)", () => {
    // Production deltas carry only DIRTY rows; cellCharAt must look up by the
    // row's .index, not array position. A char typed on a NON-zero row (the
    // common bottom-prompt case) otherwise reads the wrong cell → no confirm →
    // srtt stays 0 and Adaptive never arms. confirmedEpoch>0 is the
    // RTT-independent discriminator (wipe→0, reconcile→advances).
    clock.t = 0;
    const pe = new PredictiveEcho(fakeHost(), { now: () => clock.t });
    pe.onFrame(frame({ seq: 1, full: true, cr: 5, cc: 0, rows: [null] })); // cursor on row 5
    pe.predict(enc("a")); // a@(5,0), bornSeq 1
    clock.t = 200;
    // DELTA: only row 5 changed, at viewportRows ARRAY POSITION 0 (.index=5).
    const delta = {
      cols: 80, rows: 24, cursorRow: 5, cursorCol: 1, cursorVisible: true,
      altScreen: false, cursorKeysApp: false, bracketedPaste: false, full: false,
      viewportRows: [{ index: 5, spans: [{ text: "a", fg: 256, bg: 256, flags: 0 }] }],
      scrollbackRows: [], scrollbackAppend: [], scrollbackTotal: 0, sbBase: 0, seq: 2,
    } as CellGridFrame;
    pe.onFrame(delta);
    const d = pe._debug();
    expect(d.confirmedEpoch).toBeGreaterThan(0); // reconcile confirmed (buggy array-index → 0)
    expect(d.srtt).toBeGreaterThan(0);           // round-trip sampled
  });
});
