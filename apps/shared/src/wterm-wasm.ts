// Absolute filesystem path to the roost-patched wterm wasm
// (apps/shared/wasm/wterm-roost.wasm — fork of @wterm/core 0.3.0 raising
// MAX_SCROLLBACK_LINES 1k→10k, phase-pb9b). The worker's headless wtermCore
// loads this instead of stock @wterm/core (OPT2-2) so its serialized
// scrollback depth for alt-screen sessions (claude) matches the SPA's 10k —
// the SPA already loads the same binary via wasmUrl:"/wterm-roost.wasm".
// Resolved relative to THIS module so the path is correct wherever
// apps/shared is rsynced (deploy.ts ships apps/shared/ to every worker host).

import { fileURLToPath } from "node:url";
import { WTERM_WASM_EMBED } from "./wterm-wasm-embed.generated.ts";

// Compiled `roost` binary → embedded file path (baked by scripts/gen-embed.ts).
// From source → resolved relative to this module (rsync deploys keep layout).
export const WTERM_ROOST_WASM_PATH = WTERM_WASM_EMBED
  ?? fileURLToPath(new URL("../wasm/wterm-roost.wasm", import.meta.url));
