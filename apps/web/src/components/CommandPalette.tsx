// CommandPalette — Cmd-K modal host. Always mounted (App.tsx RootShell) but
// deliberately HOLLOW: it owns only the open signal wiring (overlayMotion
// presence) and defers the entire reactive body — memo chain, list JSX,
// keyboard nav — to CommandPaletteBody.tsx, mounted ONLY while the palette is
// open. While closed, zero computations subscribe to the store (perf sweep
// C1.1); the body chunk is also code-split and fetched on first ⌘K (C2.1).
//
// Open/close + folder context live in lib/keyboardShortcuts.ts.
// Callers: App.tsx (always rendered; gated on cmdPaletteOpen).

import { Show, lazy } from "solid-js";
import { cmdPaletteOpen } from "../lib/keyboardShortcuts.ts";
import { createOverlayPresence } from "../lib/overlayMotion.ts";

// Code-split boundary (ts-no-dynamic-import exception): solid `lazy` is the
// bundler's split mechanism — the body chunk loads on first open, keeping the
// palette's heavy deps (connect, palette data builders) out of the eager path.
const PaletteBody = lazy(() =>
  import("./CommandPaletteBody.tsx").then((m) => ({ default: m.PaletteBody })),
);

export function CommandPalette() {
  const { present, setPanelRef } = createOverlayPresence(cmdPaletteOpen, "panel");
  return (
    <Show when={present()}>
      <PaletteBody setPanelRef={setPanelRef} />
    </Show>
  );
}
