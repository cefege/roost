// Headless @wterm/core grid factory. Server-side only: the worker instantiates
// a grid for each terminal session from Roost's patched WASM.
// It is not imported by the web SPA, which loads its own core.

import { WasmBridge, type TerminalCore } from "@wterm/core";
import { WTERM_ROOST_WASM_PATH } from "./wterm-wasm.ts";
import { log } from "./log.ts";

// The patched module is compiled once per process. Keep the promise, rather
// than only the resolved module, so concurrent first sessions cannot each pay
// for a compile while the first read/compile is still in flight.
let _roostWasmModulePromise: Promise<WebAssembly.Module> | null = null;
let _roostWasmFailureLogged = false;

function roostWasmModule(): Promise<WebAssembly.Module> {
  if (!_roostWasmModulePromise) {
    _roostWasmModulePromise = Bun.file(WTERM_ROOST_WASM_PATH)
      .arrayBuffer()
      .then((bytes) => WebAssembly.compile(bytes));
  }
  return _roostWasmModulePromise;
}

function logRoostWasmFailure(error: unknown): void {
  if (_roostWasmFailureLogged) return;
  _roostWasmFailureLogged = true;
  log.warn("wterm-core-factory", "roost_wasm_load_failed_fallback_stock", {
    path: WTERM_ROOST_WASM_PATH,
    msg: error instanceof Error ? error.message : String(error),
  });
}

/** Start the shared compile without allocating a terminal core. A failed
 * patched-module preparation is deliberately non-fatal: createWtermCore()
 * retains the stock-WASM fallback. */
export async function prepareWtermCoreModule(): Promise<void> {
  try {
    await roostWasmModule();
  } catch (error) {
    logRoostWasmFailure(error);
  }
}

export async function createWtermCore(cols: number, rows: number): Promise<TerminalCore> {
  try {
    const instance = await WebAssembly.instantiate(await roostWasmModule());
    const bridge = new WasmBridge(instance);
    bridge.init(cols, rows);
    return bridge;
  } catch (error) {
    // Degraded depth beats a dead core: if the patched wasm is missing or
    // unreadable on this host, fall back to stock inline wasm (~1k lines).
    logRoostWasmFailure(error);
    const bridge = await WasmBridge.load();
    bridge.init(cols, rows);
    return bridge;
  }
}
