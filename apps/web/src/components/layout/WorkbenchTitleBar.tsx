// Desktop workbench title region: product context and quiet utility actions.
// AppShell places it above the navigation rail and editor grid.
// Command access is a compact icon action; the global shortcut remains canonical.

import { A, useLocation } from "@solidjs/router";
import { createMemo, Show } from "solid-js";
import { openCmdPalette } from "../../lib/keyboardShortcuts.ts";
import { workbenchTitle } from "../../lib/workbenchTitle.ts";
import { ROUTES } from "../../routes.ts";
import { Button } from "../Settings/md/Button.tsx";
import { Icon } from "../Settings/md/primitives.tsx";
import { BrandMark } from "../BrandMark.tsx";

export function WorkbenchTitleBar() {
  const location = useLocation();
  const context = createMemo(() => workbenchTitle(location.pathname));
  return <header class="workbench-titlebar">
    <div class="workbench-titlebar__left">
      <A class="workbench-titlebar__brand" href="/" aria-label="Roost home">
        <BrandMark size={18} />
        <span class="workbench-titlebar__product">Roost</span>
        <Show when={context() !== "Roost"}>
          <span class="workbench-titlebar__context">{context()}</span>
        </Show>
      </A>
    </div>
    <div class="workbench-titlebar__right">
      <div class="workbench-titlebar__actions">
        <Button
          class="workbench-command-center"
          variant="text"
          icon="search"
          aria-label="Open command palette"
          title="Command palette"
          onClick={openCmdPalette}
        />
        <A class="workbench-titlebar__help" href={ROUTES.HELP} aria-label="Help" title="Help">
          <Icon name="help" />
        </A>
      </div>
    </div>
  </header>;
}

