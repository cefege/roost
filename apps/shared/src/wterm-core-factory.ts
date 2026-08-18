// Headless @wterm/core grid factory. Server-side only: the worker instantiates
// a grid for each terminal session from Roost's patched WASM.
// It is not imported by the web SPA, which loads its own core.
//
// There is no stock-WASM fallback. Stock @wterm/core caps scrollback at 1,000
// lines where Roost renders 10,000, so falling back silently truncates history
// on exactly the long-output sessions that need it. A patched module that fails
// to read, verify, compile, or match the 0.3.4 bridge ABI fails worker
// readiness instead (main.ts awaits prepareWtermCoreModule() at boot).

import { WasmBridge, type TerminalCore, type UnhandledSequence } from "@wterm/core";
import { WTERM_ROOST_WASM_PATH, expectedRoostWasmSha256 } from "./wterm-wasm.ts";
import { log } from "./log.ts";

// Every WASM export @wterm/core 0.3.4's WasmBridge invokes. 0.3.4 widened the
// ABI over 0.3.0 (per-cell display width and OSC 8 link index, mouse/focus and
// synchronized-output modes, discarded-row origin, hyperlink resource state);
// a 0.3.0-era module compiles fine and then throws "is not a function" deep
// inside a session write. Checking the module's exports once, at compile time,
// turns that into one legible startup failure.
const REQUIRED_WASM_FUNCTIONS = [
  "clearDirty", "clearResponse", "getBracketedPaste", "getCellSize", "getCols",
  "getCursorCol", "getCursorKeysApp", "getCursorRow", "getCursorVisible",
  "getDebugLogCount", "getDebugLogEntrySize", "getDebugLogMax", "getDebugLogPtr",
  "getDirtyPtr", "getFocusEvents", "getGridPtr", "getHyperlinkCapacity",
  "getHyperlinkCount", "getHyperlinkRejectedCount", "getLinkIdLen", "getLinkIdPtr",
  "getLinkUriLen", "getLinkUriPtr", "getMaxCols", "getMouseSgr", "getMouseTracking",
  "getResponseLen", "getResponsePtr", "getRows", "getScrollbackCount",
  "getScrollbackDiscardedCount", "getScrollbackLine", "getScrollbackLineLen",
  "getSynchronizedOutput", "getSynchronizedOutputGeneration", "getTitleChanged",
  "getTitleLen", "getTitlePtr", "getUsingAltScreen", "getWriteBuffer", "init",
  "resizeTerminal", "writeBytes",
] as const;

// The patched module is compiled once per process. Keep the promise, rather
// than only the resolved module, so concurrent first sessions cannot each pay
// for a compile while the first read/compile is still in flight — and so a
// verification failure is reported identically to every caller instead of
// being re-attempted per session.
let _roostWasmModulePromise: Promise<WebAssembly.Module> | null = null;

function assertRoostWasmAbi(module: WebAssembly.Module): void {
  const fns = new Set<string>();
  let hasMemory = false;
  for (const entry of WebAssembly.Module.exports(module)) {
    if (entry.kind === "function") fns.add(entry.name);
    else if (entry.kind === "memory" && entry.name === "memory") hasMemory = true;
  }
  const missing: string[] = REQUIRED_WASM_FUNCTIONS.filter((name) => !fns.has(name));
  if (!hasMemory) missing.unshift("memory");
  if (missing.length > 0) {
    throw new Error(
      `${WTERM_ROOST_WASM_PATH} does not implement the @wterm/core 0.3.4 bridge ABI; `
      + `missing exports: ${missing.join(", ")}. `
      + "Rebuild it with scripts/rebuild-wterm-wasm.sh.",
    );
  }
}

/** The whole load contract: bytes must hash to the digest committed beside them
 * and the compiled module must implement every export the 0.3.4 bridge calls.
 * Either check failing throws; there is no degraded return value. */
export async function verifyRoostWasm(
  bytes: ArrayBuffer,
  expectedSha256: string,
): Promise<WebAssembly.Module> {
  const actual = new Bun.CryptoHasher("sha256").update(new Uint8Array(bytes)).digest("hex");
  if (actual !== expectedSha256) {
    throw new Error(
      `${WTERM_ROOST_WASM_PATH} does not match its committed digest: expected sha256 `
      + `${expectedSha256}, got ${actual} (${bytes.byteLength} bytes). `
      + "Run scripts/rebuild-wterm-wasm.sh --verify.",
    );
  }
  const module = await WebAssembly.compile(bytes);
  assertRoostWasmAbi(module);
  return module;
}

async function compileRoostWasm(): Promise<WebAssembly.Module> {
  const [bytes, expected] = await Promise.all([
    Bun.file(WTERM_ROOST_WASM_PATH).arrayBuffer(),
    expectedRoostWasmSha256(),
  ]);
  const module = await verifyRoostWasm(bytes, expected);
  log.info("wterm-core-factory", "roost_wasm_verified", {
    path: WTERM_ROOST_WASM_PATH,
    bytes: bytes.byteLength,
    sha256: expected,
  });
  return module;
}

function roostWasmModule(): Promise<WebAssembly.Module> {
  if (!_roostWasmModulePromise) _roostWasmModulePromise = compileRoostWasm();
  return _roostWasmModulePromise;
}

/** Read, verify and compile the patched module without allocating a terminal
 * core, so the process pays the ~32 MiB-per-instance cost only per session and
 * an unusable core is a boot failure rather than a first-session surprise.
 * Rejects on digest mismatch, ABI mismatch, or unreadable/uncompilable bytes. */
export async function prepareWtermCoreModule(): Promise<void> {
  await roostWasmModule();
}

// ─── Unhandled-sequence ring ───────────────────────────────────────────────
//
// The core keeps a 32-entry ring of CSI sequences its dispatcher ignored
// (`DebugLogEntry`, terminal.zig). It is the only telemetry that exists for "the
// core silently dropped this sequence", the class behind "my TUI renders wrong in
// Roost but fine in iTerm".
//
// 0.3.4's own bridge decodes that ring at the WRONG OFFSETS. `DebugLogEntry` is a
// PLAIN Zig struct, so the compiler orders its `[4]u16 params` first and the three
// u8s after them, while wasm-bridge.js reads `final` at +0, `private` at +1,
// `paramCount` at +2 and the params from +4. Every entry it returns is a
// parameter's low byte wearing the final byte's name: `CSI 2 SP q` (DECSCUSR)
// comes back as final "\x02" with no params, and because the bogus paramCount is
// then 0 the real final ('q') is not reachable from the returned object at all.
// The layout below is the one the digest-pinned module actually has, verified
// end-to-end in apps/shared/tests/wterm-unhandled-sequences.test.ts; a rebuild
// that reordered the struct would fail that test rather than quietly lie.
const ENTRY_PARAMS_OFF = 0; // [4]u16, little-endian
const ENTRY_PARAM_SLOTS = 4;
const ENTRY_FINAL_OFF = 8; // u8
const ENTRY_PRIVATE_OFF = 9; // u8
const ENTRY_PARAM_COUNT_OFF = 10; // u8 — may exceed ENTRY_PARAM_SLOTS

/** Ring accounting the `TerminalCore` interface cannot express. `total` is every
 *  sequence this core has EVER logged (the ring only retains the newest
 *  `capacity`), which is what lets a consumer hold an exact high-water mark
 *  against a ring that is never cleared and count what it overwrote unseen. */
export interface UnhandledSequenceRing {
  readonly capacity: number;
  total(): number;
  /** The retained window, oldest → newest. Entry i is logical sequence
   *  `total() - min(total(), capacity) + i`. */
  entries(): UnhandledSequence[];
}

type WasmCounter = () => number;

function isWasmCounter(value: WebAssembly.ExportValue): value is WasmCounter {
  return typeof value === "function";
}

function wasmCounter(instance: WebAssembly.Instance, name: string): WasmCounter {
  const fn = instance.exports[name];
  // assertRoostWasmAbi already proved every name is an exported function; this
  // keeps the narrowing honest instead of asserting a type.
  if (fn === undefined || !isWasmCounter(fn)) {
    throw new Error(`${WTERM_ROOST_WASM_PATH} export ${name} is not callable`);
  }
  return fn;
}

function makeUnhandledSequenceRing(instance: WebAssembly.Instance): UnhandledSequenceRing {
  const memory = instance.exports.memory;
  if (!(memory instanceof WebAssembly.Memory)) {
    throw new Error(`${WTERM_ROOST_WASM_PATH} exports no linear memory`);
  }
  const logCount = wasmCounter(instance, "getDebugLogCount");
  const logPtr = wasmCounter(instance, "getDebugLogPtr");
  const logEntrySize = wasmCounter(instance, "getDebugLogEntrySize");
  const capacity = wasmCounter(instance, "getDebugLogMax")();
  return {
    capacity,
    total: logCount,
    entries(): UnhandledSequence[] {
      const total = logCount();
      if (total === 0) return [];
      const retained = Math.min(total, capacity);
      const size = logEntrySize();
      const ptr = logPtr();
      // The write index is total % capacity, so a wrapped ring's oldest retained
      // entry sits there; below capacity it starts at 0. Fresh DataView per read:
      // a memory growth detaches the old buffer.
      const start = total >= capacity ? total % capacity : 0;
      const dv = new DataView(memory.buffer);
      const out: UnhandledSequence[] = new Array<UnhandledSequence>(retained);
      for (let i = 0; i < retained; i++) {
        const off = ptr + ((start + i) % capacity) * size;
        const paramCount = dv.getUint8(off + ENTRY_PARAM_COUNT_OFF);
        const privateByte = dv.getUint8(off + ENTRY_PRIVATE_OFF);
        const slots = paramCount < ENTRY_PARAM_SLOTS ? paramCount : ENTRY_PARAM_SLOTS;
        const params: number[] = new Array<number>(slots);
        for (let p = 0; p < slots; p++) {
          params[p] = dv.getUint16(off + ENTRY_PARAMS_OFF + p * 2, true);
        }
        out[i] = {
          final: String.fromCharCode(dv.getUint8(off + ENTRY_FINAL_OFF)),
          private: privateByte === 0 ? "" : String.fromCharCode(privateByte),
          paramCount,
          params,
        };
      }
      return out;
    },
  };
}

// Keyed by the core it was built for, so `TerminalCore` stays the type every
// consumer holds. A core built any other way (a bare WasmBridge in a test
// double) has no reader and reports nothing rather than the bridge's mis-decode.
const _unhandledRings = new WeakMap<TerminalCore, UnhandledSequenceRing>();

/** The ring reader for a core created by `createWtermCore`, else null. */
export function unhandledSequenceRing(core: TerminalCore): UnhandledSequenceRing | null {
  return _unhandledRings.get(core) ?? null;
}

/** One independent, mutable core per session or rebuild, instantiated from the
 * shared stateless module. Each instance owns its own linear memory (the 10k ×
 * 256 × 12-byte scrollback reserves ~32 MiB), so cores never share grid,
 * scrollback, response-queue or link-table state; dropping the last reference
 * to a bridge releases that memory (the ABI exposes no explicit teardown). */
export async function createWtermCore(cols: number, rows: number): Promise<TerminalCore> {
  const instance = await WebAssembly.instantiate(await roostWasmModule());
  const bridge = new WasmBridge(instance);
  bridge.init(cols, rows);
  const ring = makeUnhandledSequenceRing(instance);
  _unhandledRings.set(bridge, ring);
  // Leave no second, wrong way to read the same ring: the interface method now
  // decodes at the real offsets too (see the layout note above).
  bridge.getUnhandledSequences = ring.entries;
  return bridge;
}
