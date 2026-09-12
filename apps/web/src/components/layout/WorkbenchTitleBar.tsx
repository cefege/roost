// Desktop workbench title region: product and route context.
// AppShell places it above the navigation rail and editor grid.
// Command-palette access remains keyboard-only.

import { A, useLocation } from "@solidjs/router";
import { createMemo, Show } from "solid-js";
import { workbenchTitle } from "../../lib/workbenchTitle.ts";
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
  </header>;
}

