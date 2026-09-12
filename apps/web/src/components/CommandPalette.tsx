// CommandPalette — lazy command/search body within the shared responsive Sheet.
// Its open signal lives in keyboardShortcuts; the body exists only while open.
// Closing remains immediate so the selected command can navigate without an overlay delay.

import { Show, lazy } from "solid-js";
import { isCompact } from "../lib/windowSizeClass.ts";
import { closeCmdPalette, cmdPaletteOpen } from "../lib/keyboardShortcuts.ts";
import { Sheet } from "./Settings/md/Sheet.tsx";

const PaletteBody = lazy(() =>
  import("./CommandPaletteBody.tsx").then((module) => ({ default: module.PaletteBody })),
);

export function CommandPalette() {
  return (
    <Sheet
      open={cmdPaletteOpen()}
      onClose={closeCmdPalette}
      headline="Command palette"
      side={isCompact() ? "bottom" : "center"}
      class="roost-dialog--wide roost-dialog--command-palette"
      onOpenAutoFocus={(event) => event.preventDefault()}
    >
      <Show when={cmdPaletteOpen()}>
        <PaletteBody />
      </Show>
    </Sheet>
  );
}
