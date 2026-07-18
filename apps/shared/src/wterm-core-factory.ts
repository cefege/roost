// Headless @wterm/core grid factory. Server-side only (Bun): the worker
// session-manager and the coord claude-status-hub each instantiate
// per-session grids from the roost-patched WASM (10k scrollback cap).
// NOT imported by the web SPA — the browser loads its own core via
// RoostTerm. Kept out of the @roost/shared root index for that reason;
// import via the "@roost/shared/wterm-core-factory" subpath.
//
// Extracted from worker session-manager._createWtermCore so coord reuses
// the identical grid semantics it must scrape (CLAUDE.md standard #10 —
// no parallel utility).

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
