// CommandPalette — Cmd-K modal host. Mounted inside App.tsx's protected
// overlay shell but deliberately HOLLOW: it owns only the open signal wiring (overlayMotion
// presence) and defers the entire reactive body — memo chain, list JSX,
// keyboard nav — to CommandPaletteBody.tsx, mounted ONLY while the palette is
// open. While closed, zero computations subscribe to the store (perf sweep
// C1.1); the body chunk is also code-split and fetched on first ⌘K (C2.1).
//
// Open/close + folder context live in lib/keyboardShortcuts.ts.
// Callers: App.tsx (mounted after protected route access; gated on cmdPaletteOpen).

import { Show, createEffect, lazy } from "solid-js";
import { cmdPaletteOpen } from "../lib/keyboardShortcuts.ts";
import { createOverlayPresence } from "../lib/overlayMotion.ts";

// Code-split boundary (ts-no-dynamic-import exception): solid `lazy` is the
// bundler's split mechanism — the body chunk loads on first open, keeping the
// palette's heavy deps (connect, palette data builders) out of the eager path.
const PaletteBody = lazy(() =>
  import("./CommandPaletteBody.tsx").then((m) => ({ default: m.PaletteBody })),
);

export function CommandPalette() {
  let restoreTarget: HTMLElement | null = null;
  let paletteWasOpen = false;
  const { present, setPanelRef } = createOverlayPresence(cmdPaletteOpen, "panel");
  createEffect(() => {
    const open = cmdPaletteOpen();
    if (open) {
      if (!paletteWasOpen) {
        restoreTarget = document.activeElement instanceof HTMLElement ? document.activeElement : null;
        paletteWasOpen = true;
      }
      return;
    }
    if (!paletteWasOpen) return;
    const target = restoreTarget;
    restoreTarget = null;
    paletteWasOpen = false;
    queueMicrotask(() => {
      if (target?.isConnected) target.focus();
    });
  });
  return (
    <Show when={present()}>
      <PaletteBody setPanelRef={setPanelRef} />
    </Show>
  );
}
