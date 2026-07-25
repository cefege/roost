// cell-phase-2 real-flow — drive a REAL @wterm/core VT engine, emit full+delta via
// the worker's exact decision path (nextCellFrame), and assert the client's
// applyDelta reconstructs the same grid a fresh full read produces. This is
// the cell-layer analog of the byte-fidelity tests: real parse → cells →
// delta → reconstruct, no mock VT. If isDirtyRow/scrollback handling drifts,
// the reconstruct diverges from the fresh read.

import { describe, test, expect } from "bun:test";
import { WasmBridge } from "@wterm/core";
import {
  nextCellFrame, initCellEmitState, gridToCellFrame, applyDelta,
} from "../src/cell/index.ts";

describe("real-core full → delta → applyDelta reconstruct", () => {
  test("scrolling shell output: client reconstruction equals a fresh full read", async () => {
    const core = await WasmBridge.load();
    core.init(20, 4);
    core.writeRaw(new TextEncoder().encode("AAAA\r\nBBBB\r\nCCCC\r\n"));

    let st = initCellEmitState();
    const full = nextCellFrame(core, st, false);
    expect(full.frame.full).toBe(true);
    const seen = full.frame.viewportRows.flatMap((r) => r.spans.map((s) => s.text)).join("|");
    expect(seen).toContain("AAAA");
    expect(seen).toContain("CCCC");
    core.clearDirty();

    // Push more lines than fit → older rows scroll into scrollback.
    core.writeRaw(new TextEncoder().encode("DDDD\r\nEEEE\r\nFFFF\r\nGGGG\r\nHHHH\r\n"));
    const delta = nextCellFrame(core, full.state, false);
    expect(delta.frame.full).toBe(false);
    expect(core.getScrollbackCount()).toBeGreaterThan(0);
    expect(delta.frame.scrollbackAppend.length).toBe(core.getScrollbackCount());

    const reconstructed = applyDelta(full.frame, delta.frame);
    const freshFull = gridToCellFrame(core, delta.frame.seq);
    expect(reconstructed).toEqual(freshFull);
  });

  // "Proper" content — a colored box-drawing markdown table + bullets, the
  // shape a real `claude --model haiku` session paints (verified live
  // 2026-06-22: cell mode renders these tables, A→B→A settles byte-identical).
  // Box-drawing + 256-color + truecolor + bullets through the cell pipeline.
  const TABLE_BYTES = new TextEncoder().encode(
    "\x1b[1;36m┌────────────┬────────────┬──────────────┐\r\n" +
    "│ \x1b[0;1mMethod\x1b[0;1;36m     │ \x1b[0;1mBrew Time\x1b[0;1;36m  │ \x1b[0mGrind\x1b[1;36m        │\r\n" +
    "├────────────┼────────────┼──────────────┤\r\n" +
    "│ \x1b[0;38;5;208mFrench Press\x1b[0;1;36m │ \x1b[0m4 minutes\x1b[1;36m  │ \x1b[38;2;120;200;80mCoarse\x1b[0;1;36m       │\r\n" +
    "│ \x1b[0;38;5;208mEspresso\x1b[0;1;36m     │ \x1b[0m30 seconds\x1b[1;36m │ \x1b[38;2;120;200;80mFine\x1b[0;1;36m         │\r\n" +
    "└────────────┴────────────┴──────────────┘\x1b[0m\r\n" +
    "\x1b[32m• Pro:\x1b[0m rich flavor, full body\r\n" +
    "\x1b[31m• Con:\x1b[0m sediment in the cup\r\n",
  );

  test("colored box-drawing table + bullets render to faithful cell spans", async () => {
    const core = await WasmBridge.load();
    core.init(48, 12);
    core.writeRaw(TABLE_BYTES);
    const f = gridToCellFrame(core, 1);
    const allText = f.viewportRows.flatMap((r) => r.spans.map((s) => s.text)).join("");
    // Box-drawing preserved verbatim (no reflow/mangle).
    for (const ch of ["┌", "┬", "┐", "│", "├", "┼", "┤", "└", "┴", "┘", "─"]) {
      expect(allText).toContain(ch);
    }
    expect(allText).toContain("French Press");
    expect(allText).toContain("• Pro:");
    // Colors land on spans: 256-color orange (208) on "French Press",
    // truecolor green on "Coarse", ANSI green on the bullet.
    const spans = f.viewportRows.flatMap((r) => r.spans);
    expect(spans.some((s) => s.text.includes("French Press") && s.fg === 208)).toBe(true);
    // truecolor: stock core may resolve fg_rgb OR a palette fg — either way
    // "Coarse" must carry a non-default foreground (color survived the parse).
    const coarse = spans.find((s) => s.text.includes("Coarse"));
    expect(coarse).toBeTruthy();
    expect(coarse!.fgRgb !== undefined || coarse!.fg !== 256).toBe(true);
    expect(spans.some((s) => s.text.includes("Pro:") && s.fg === 2)).toBe(true);
  });

  test("rich table: full → delta → applyDelta reconstructs exactly", async () => {
    const core = await WasmBridge.load();
    core.init(48, 14);
    core.writeRaw(TABLE_BYTES);
    const full = nextCellFrame(core, initCellEmitState(), false);
    core.clearDirty();
    core.writeRaw(new TextEncoder().encode("\x1b[33m• Note:\x1b[0m grind matters most\r\n"));
    const delta = nextCellFrame(core, full.state, false);
    expect(applyDelta(full.frame, delta.frame)).toEqual(gridToCellFrame(core, delta.frame.seq));
  });

  test("determinism: two independent cores, same bytes + cols → identical cell frame", async () => {
    // The worker re-renders on resize by rebuilding a fresh core from the raw
    // ring (NOT in-place resize). So gridToCellFrame must be a pure function of
    // (bytes, cols) — this is what makes A→B→A settle byte-identical live.
    const a = await WasmBridge.load(); a.init(60, 16); a.writeRaw(TABLE_BYTES);
    const b = await WasmBridge.load(); b.init(60, 16); b.writeRaw(TABLE_BYTES);
    expect(gridToCellFrame(a, 1)).toEqual(gridToCellFrame(b, 1));
  });

  // Phase-2 (G5): grapheme / wide-char fidelity through the cell pipeline.
  // The Zig core is the lightweight VT (README defers full grapheme handling
  // to a heavier core), so this pins what cell mode actually delivers for CJK + emoji
  // before byte mode is retired — a regression here is the cue to switch the
  // worker core to a full grapheme-aware core.
  test("CJK + emoji survive real-core parse → cell spans (wide-char width preserved)", async () => {
    const core = await WasmBridge.load();
    core.init(40, 6);
    // CJK (each is width-2), an emoji, then ASCII on the next line.
    core.writeRaw(new TextEncoder().encode("你好世界 \u{1f600}\r\nASCII-TAIL\r\n"));
    const f = gridToCellFrame(core, 1);
    const allText = f.viewportRows.flatMap((r) => r.spans.map((s) => s.text)).join("");
    // Wide CJK glyphs preserved verbatim (not dropped / not split into mojibake).
    for (const ch of ["你", "好", "世", "界"]) {
      expect(allText).toContain(ch);
    }
    // The following ASCII line is intact → the wide chars consumed exactly
    // their cells and didn't desync the grid.
    expect(allText).toContain("ASCII-TAIL");
  });

  test("alt-screen toggle forces a full reframe (TUI redraw)", async () => {
    const core = await WasmBridge.load();
    core.init(20, 4);
    core.writeRaw(new TextEncoder().encode("shell prompt $ "));
    const full = nextCellFrame(core, initCellEmitState(), false);
    core.clearDirty();
    // Enter alt-screen (what a TUI like vim/claude does).
    core.writeRaw(new Uint8Array([0x1b, 0x5b, 0x3f, 0x31, 0x30, 0x34, 0x39, 0x68])); // ESC[?1049h
    core.writeRaw(new TextEncoder().encode("TUI"));
    const next = nextCellFrame(core, full.state, false);
    expect(core.usingAltScreen()).toBe(true);
    expect(next.frame.full).toBe(true);
    expect(next.frame.altScreen).toBe(true);
  });

  // The ring is bounded. Once it saturates getScrollbackCount() PINS, and the
  // old "N-th oldest retained" index origin then slid by one on every eviction:
  // appends computed from the count delta went empty (the SPA's scrollback
  // simply stopped growing), the reframe predicate `total < lastSbTotal` never
  // fired, and every absolute index the SPA held silently re-aliased to a
  // different line. Content is uniform-width `CELLLINE-%d` on purpose — that is
  // the workload that saturates a ring, and a length-only tail probe aliases
  // every line of it to the same signature.
  test("scrollback keeps flowing and indices stay stable after the ring saturates", async () => {
    const core = await WasmBridge.load();
    core.init(80, 24);
    const enc = new TextEncoder();
    let next = 1;
    const write = (n: number): void => {
      for (let i = 0; i < n; i++) core.writeRaw(enc.encode(`CELLLINE-${next++}\r\n`));
    };
    let st = initCellEmitState();
    const emit = () => {
      const r = nextCellFrame(core, st, false);
      st = r.state;
      core.clearDirty();
      return r.frame;
    };
    // Chunks of 100 — what one coalesce window realistically ships, and well
    // inside SB_SHIFT_SCAN_MAX so the shift resolves instead of reframing.
    const CHUNK = 100;
    for (let b = 0; b < 20; b++) { write(CHUNK); emit(); }
    const retained = core.getScrollbackCount();
    write(CHUNK); emit();
    expect(core.getScrollbackCount()).toBe(retained); // the count has PINNED

    // Locate a still-retained line by its monotonic index.
    const rowsOf = (f: { scrollbackRows: { index: number; spans: { text: string }[] }[] }) =>
      f.scrollbackRows.map((r) => [r.index, r.spans.map((s) => s.text).join("")] as const);
    const marker = `CELLLINE-${next - 50}`;
    const beforeRows = rowsOf(gridToCellFrame(core, 0, undefined, st.sbDropped));
    const beforeIdx = beforeRows.find(([, t]) => t === marker)?.[0];
    expect(beforeIdx).toBeDefined();
    const totalBefore = st.lastSbTotal;

    // Three more windows of output — 300 lines the pinned count cannot see.
    let appended = 0;
    for (let b = 0; b < 3; b++) {
      write(CHUNK);
      const f = emit();
      expect(f.full).toBe(false); // the shift resolved; no reframe needed
      appended += f.scrollbackAppend.length;
    }
    expect(appended).toBe(3 * CHUNK);        // pre-fix: 0 — history stopped flowing
    expect(st.lastSbTotal).toBe(totalBefore + 3 * CHUNK); // pre-fix: unchanged

    const afterRows = rowsOf(gridToCellFrame(core, 0, undefined, st.sbDropped));
    const afterIdx = afterRows.find(([, t]) => t === marker)?.[0];
    expect(afterIdx).toBe(beforeIdx!);       // pre-fix: slid down by 300
    // And the index still names that line, not merely an equal number.
    expect(afterRows.find(([i]) => i === beforeIdx)?.[1]).toBe(marker);
  });
});
