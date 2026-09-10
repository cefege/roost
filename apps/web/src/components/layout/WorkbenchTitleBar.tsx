// Desktop workbench title region: product context and the real command palette entry.
// AppShell places it above the navigation rail and editor grid.
// No native-window controls or copied desktop-shell assets live here.

import { A, useLocation } from "@solidjs/router";
import { createMemo } from "solid-js";
import { openCmdPalette } from "../../lib/keyboardShortcuts.ts";
import { platformShortcutLabel } from "../../lib/browserPlatform.ts";
import { workbenchTitle } from "../../lib/workbenchTitle.ts";
import { ROUTES } from "../../routes.ts";
import { Button } from "../Settings/md/Button.tsx";
import { Icon } from "../Settings/md/primitives.tsx";
import { BrandMark } from "../BrandMark.tsx";
export function WorkbenchTitleBar() {
  const location = useLocation();
  const context = createMemo(() => workbenchTitle(location.pathname));
  return <header class="workbench-titlebar">
    <A class="workbench-titlebar__brand" href="/" aria-label="Roost home">
      <BrandMark size={18} />
      <span class="workbench-titlebar__product">Roost</span>
      <span class="workbench-titlebar__context">{context()}</span>
    </A>
    <Button
      class="workbench-command-center"
      variant="text"
      icon="search"
      aria-label="Open command center"
      onClick={openCmdPalette}
    >
      <span class="workbench-command-center__label">Command Center</span>
      <kbd>{platformShortcutLabel("commandPalette", "⌘K")}</kbd>
    </Button>
    <div class="workbench-titlebar__actions">
      <A class="workbench-titlebar__help" href={ROUTES.HELP} aria-label="Help" title="Help">
        <Icon name="help" />
      </A>
    </div>
  </header>;
}
