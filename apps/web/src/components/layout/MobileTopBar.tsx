// Mobile top app bar — a slim bar above the main
// content on compact: menu button (opens the sliding drawer) + the current
// context's title. Replaces the crude floating hamburger box. Rendered by
// AppShell inside <main> (a flex column) so the terminal flows below it — no
// fixed positioning, no occlusion of the input line.
//
// Owners: AppShell.tsx. Depends on: uiStore (openSidebar), rootStore.

import { createMemo } from "solid-js";
import { useLocation } from "@solidjs/router";
import { openSidebar } from "../../store/uiStore.ts";
import { workbenchTitle } from "../../lib/workbenchTitle.ts";
import { IconButton } from "../Settings/md/primitives.tsx";


export function MobileTopBar() {
  const location = useLocation();

  const title = createMemo(() => workbenchTitle(location.pathname));

  return (
    <header data-testid="mobile-topbar" class="mobile-topbar">
      <IconButton
        class="mobile-topbar__menu"
        icon="menu"
        label="Open sidebar"
        data-testid="mobile-topbar-menu"
        onClick={openSidebar}
      />
      <span class="mobile-topbar__title">{title()}</span>
    </header>
  );
}
