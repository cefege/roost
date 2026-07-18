// OPT2-2 guard: the worker loads the roost-patched wterm wasm
// (MAX_SCROLLBACK_LINES 1k→10k) so a serialized alt-screen snapshot carries
// the full depth, matching the SPA. Proven by contrast: the patched core
// retains >1k scrollback lines after 5000 lines of output; stock @wterm/core
// 0.3.0 (the inline base64) caps well below that.

import { test, expect } from "bun:test";
import { WasmBridge } from "@wterm/core";
import { WTERM_ROOST_WASM_PATH } from "@roost/shared/wterm-wasm";

const FIVE_K_LINES =
  Array.from({ length: 5000 }, (_, i) => `line ${i}`).join("\r\n") + "\r\n";

test("roost-patched wasm loads from the shipped path + retains >1k scrollback lines", async () => {
  const bytes = await Bun.file(WTERM_ROOST_WASM_PATH).arrayBuffer();
  const mod = await WebAssembly.compile(bytes);
  const inst = (await WebAssembly.instantiate(mod)) as WebAssembly.Instance;
  const core = new WasmBridge(inst);
  core.init(80, 24);
  core.writeRaw(new TextEncoder().encode(FIVE_K_LINES));
  expect(core.getScrollbackCount()).toBeGreaterThan(1000);
});

test("stock inline wasm caps well below — proves the patch is materially in effect", async () => {
  const core = await WasmBridge.load();
  core.init(80, 24);
  core.writeRaw(new TextEncoder().encode(FIVE_K_LINES));
  expect(core.getScrollbackCount()).toBeLessThan(2000);
});
