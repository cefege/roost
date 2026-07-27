// Headless @wterm/core grid factory. Server-side only: the worker instantiates
// a grid for each terminal session from Roost's patched WASM.
// It is not imported by the web SPA, which loads its own core.

import { WasmBridge, type TerminalCore } from "@wterm/core";
import { WTERM_ROOST_WASM_PATH } from "./wterm-wasm.ts";
import { log } from "./log.ts";

// Compiled once per process, instantiated per core.
let _roostWasmModule: WebAssembly.Module | null = null;

export async function createWtermCore(cols: number, rows: number): Promise<TerminalCore> {
  try {
    if (!_roostWasmModule) {
      const bytes = await Bun.file(WTERM_ROOST_WASM_PATH).arrayBuffer();
      _roostWasmModule = await WebAssembly.compile(bytes);
    }
    const instance = await WebAssembly.instantiate(_roostWasmModule);
    const bridge = new WasmBridge(instance);
    bridge.init(cols, rows);
    return bridge;
  } catch (e) {
    // Degraded depth beats a dead core: if the patched wasm is missing or
    // unreadable on this host, fall back to stock inline wasm (~1k lines).
    log.warn("wterm-core-factory", "roost_wasm_load_failed_fallback_stock", {
      path: WTERM_ROOST_WASM_PATH, msg: e instanceof Error ? e.message : String(e),
    });
    const bridge = await WasmBridge.load();
    bridge.init(cols, rows);
    return bridge;
  }
}
