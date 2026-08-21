// Plan section 7 — the patched-core load contract.
//
// The worker runs a LOCALLY PATCHED @wterm/core wasm (upstream 0.3.4 plus
// scripts/wterm-0.3.4-roost.patch). Two things used to be able to rot
// silently: the artifact could drift from the sources that built it, and a
// stale/foreign module could be loaded anyway because the factory degraded to
// the stock 1k-line core on any failure. Both are now hard failures, and this
// suite is what keeps them hard — a future "just log and carry on" edit here
// reopens the truncated-history class it was supposed to close.
//
// It also pins the per-session memory envelope: the 10k-line ring is a static
// reservation in the module, so every session costs it up front.

import { describe, test, expect } from "bun:test";
import { WasmBridge, type TerminalCore } from "@wterm/core";
import {
  assertWtermCoreGeometry, assertWtermCoreModuleLimits,
  createWtermCore, prepareWtermCoreModule, resizeWtermCore, verifyRoostWasm,
} from "../src/wterm-core-factory.ts";
import { WTERM_ROOST_WASM_PATH, expectedRoostWasmSha256 } from "../src/wterm-wasm.ts";
import {
  TERMINAL_MAX_COLS, TERMINAL_MAX_ROWS, TERMINAL_VIEW_HEARTBEAT_MS,
  TERMINAL_VIEW_LEASE_MS, TERMINAL_VIEW_SWEEP_MS, clampTerminalGeometry,
  minimumTerminalGeometry,
} from "../src/viewport.ts";

const artifact = await Bun.file(WTERM_ROOST_WASM_PATH).arrayBuffer();
const committedSha256 = await expectedRoostWasmSha256();

// 522 wasm pages. MAX_SCROLLBACK_LINES(10_000) x MAX_COLS(256) x 12-byte cells
// is 30,720,000 bytes of it; the rest is the live grid, the hyperlink table and
// the parser's own statics. Every session instantiates this much.
const INSTANCE_BYTES = 522 * 65_536;

const sha256 = (bytes: ArrayBuffer): string =>
  new Bun.CryptoHasher("sha256").update(new Uint8Array(bytes)).digest("hex");

function rowText(core: TerminalCore, row: number, cols: number): string {
  let out = "";
  for (let col = 0; col < cols; col++) {
    const cell = core.getCell(row, col);
    out += cell.chars ?? (cell.char === 0 ? "" : String.fromCodePoint(cell.char));
  }
  return out.trimEnd();
}

function instanceMemory(instance: WebAssembly.Instance): WebAssembly.Memory {
  const memory = instance.exports.memory;
  if (!(memory instanceof WebAssembly.Memory)) {
    throw new Error("verified module exported no linear memory");
  }
  return memory;
}

describe("patched wterm wasm load contract", () => {
  test("the committed artifact matches its committed digest", async () => {
    expect(sha256(artifact)).toBe(committedSha256);
    // Resolving proves the same bytes also carry the full 0.3.4 export set.
    await expect(verifyRoostWasm(artifact, committedSha256))
      .resolves.toBeInstanceOf(WebAssembly.Module);
  });

  test("a single flipped byte is refused instead of loaded", async () => {
    const tampered = artifact.slice(0);
    const view = new Uint8Array(tampered);
    const at = view.length >> 1;
    view[at] = view[at]! ^ 0xff;
    await expect(verifyRoostWasm(tampered, committedSha256)).rejects.toThrow(
      /does not match its committed digest[\s\S]*rebuild-wterm-wasm\.sh/,
    );
  });

  test("a module without the 0.3.4 bridge ABI is refused by export name", async () => {
    // A well-formed but empty module: correct magic + version, no exports. Its
    // digest is honest, so only the ABI check can reject it.
    const empty = new ArrayBuffer(8);
    new Uint8Array(empty).set([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
    const failure = await verifyRoostWasm(empty, sha256(empty)).then(
      () => null,
      (error: unknown) => (error instanceof Error ? error.message : String(error)),
    );
    expect(failure).toContain("does not implement the @wterm/core 0.3.4 bridge ABI");
    // Named, not counted: the operator needs to know which contract broke.
    for (const name of ["memory", "getScrollbackDiscardedCount", "getMouseTracking", "writeBytes"]) {
      expect(failure).toContain(name);
    }
  });

  test("the loaded core answers the whole 0.3.4 surface, not just the 0.3.0 subset", async () => {
    await prepareWtermCoreModule();
    const core = await createWtermCore(80, 24);
    // Present-and-callable, which is what a compiled-but-wrong module fails.
    expect(core.getScrollbackDiscardedCount?.()).toBe(0);
    expect(core.mouseTracking?.()).toBe(0);
    core.writeString("\x1b[?1002h\x1b[?1006h\x1b[?1004h");
    expect(core.mouseTracking?.()).toBe(1002);
    expect(core.mouseSgr?.()).toBe(true);
    expect(core.focusEvents?.()).toBe(true);
    const generation = core.synchronizedOutputGeneration?.() ?? -1;
    core.writeString("\x1b[?2026h");
    expect(core.synchronizedOutput?.()).toBe(true);
    core.writeString("\x1b[?2026l");
    expect(core.synchronizedOutputGeneration?.()).toBeGreaterThan(generation);
    core.writeString("\x1b]8;id=a1;https://example.test/x\x07L\x1b]8;;\x07");
    expect(core.getCell(0, 0).linkUri).toBe("https://example.test/x");
    expect(core.getCell(0, 0).linkId).toBe("a1");
    expect(core.getResourceState?.().hyperlinks?.capacity).toBeGreaterThan(0);
  });

  test("shared 256x256 bounds match the core clamp and validated factory contract", async () => {
    const module = await verifyRoostWasm(artifact, committedSha256);
    const instance = await WebAssembly.instantiate(module);
    expect(() => assertWtermCoreModuleLimits(instance)).not.toThrow();
    const raw = new WasmBridge(instance);
    raw.init(TERMINAL_MAX_COLS + 1, TERMINAL_MAX_ROWS + 1);
    expect(raw.getCols()).toBe(TERMINAL_MAX_COLS);
    expect(raw.getRows()).toBe(TERMINAL_MAX_ROWS);

    const core = await createWtermCore(TERMINAL_MAX_COLS, TERMINAL_MAX_ROWS);
    expect(() => assertWtermCoreGeometry(core, {
      cols: TERMINAL_MAX_COLS,
      rows: TERMINAL_MAX_ROWS,
    })).not.toThrow();
    resizeWtermCore(core, { cols: 80, rows: 24 });
    expect([core.getCols(), core.getRows()]).toEqual([80, 24]);
    await expect(createWtermCore(TERMINAL_MAX_COLS + 1, 24)).rejects.toThrow(/outside/);
    await expect(createWtermCore(80, TERMINAL_MAX_ROWS + 1)).rejects.toThrow(/outside/);
  });

  test("shared view policy clamps measurements and minimizes each axis independently", () => {
    expect(clampTerminalGeometry({ cols: 257, rows: 1_000 })).toEqual({
      cols: TERMINAL_MAX_COLS,
      rows: TERMINAL_MAX_ROWS,
    });
    expect(minimumTerminalGeometry([
      { cols: 80, rows: 50 },
      { cols: 120, rows: 24 },
    ])).toEqual({ cols: 80, rows: 24 });
    expect(minimumTerminalGeometry([])).toBeNull();
    expect([
      TERMINAL_VIEW_LEASE_MS,
      TERMINAL_VIEW_HEARTBEAT_MS,
      TERMINAL_VIEW_SWEEP_MS,
    ]).toEqual([15_000, 5_000, 1_000]);
  });

  test("cores from the shared module share no mutable state", async () => {
    const a = await createWtermCore(80, 24);
    const b = await createWtermCore(80, 24);
    a.writeString("alpha\r\n".repeat(40));
    b.writeString("beta\r\n");
    expect(rowText(a, 0, 80)).toBe("alpha");
    expect(rowText(b, 0, 80)).toBe("beta");
    expect(a.getScrollbackCount()).toBeGreaterThan(0);
    expect(b.getScrollbackCount()).toBe(0);
    // The response queue is per instance too: draining one must not consume the
    // other's reply, and must not hand it a reply it never asked for.
    a.writeRaw(new TextEncoder().encode("\x1b[6n"));
    expect(b.getResponse()).toBeNull();
    expect(a.getResponse()).toBe("\x1b[24;1R");
    expect(a.getResponse()).toBeNull();
    // A rebuild replaces one session's core without disturbing its neighbour.
    const rebuilt = await createWtermCore(100, 30);
    expect(rebuilt.getCols()).toBe(100);
    expect(rowText(b, 0, 80)).toBe("beta");
  });

  test("per-session memory is the patched reservation and teardown reclaims it", async () => {
    const module = await verifyRoostWasm(artifact, committedSha256);
    const first = instanceMemory(await WebAssembly.instantiate(module));
    const second = instanceMemory(await WebAssembly.instantiate(module));
    // Each instantiation owns its own linear memory of exactly the patched
    // size. If this number moves, the supported-session envelope moved with it.
    expect(first.buffer.byteLength).toBe(INSTANCE_BYTES);
    expect(second.buffer.byteLength).toBe(INSTANCE_BYTES);
    expect(first.buffer).not.toBe(second.buffer);

    Bun.gc(true);
    const before = process.memoryUsage.rss();
    for (let i = 0; i < 64; i++) {
      const core = await createWtermCore(80, 24);
      core.writeString(`soak ${i}\r\n`.repeat(64));
      core.resize(100, 30);
    }
    Bun.gc(true);
    // 64 leaked instances would be ~2 GiB. Two instances' worth of slack is
    // generous for allocator noise and still catches a real retain.
    expect(process.memoryUsage.rss() - before).toBeLessThan(2 * INSTANCE_BYTES);
  });
});
