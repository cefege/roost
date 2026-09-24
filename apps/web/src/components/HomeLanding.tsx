// Home landing ("hello" pane) rendered at "/" when workers exist but no
// session is auto-opened. Chrome-new-tab-page-style empty state: brand row,
// keyboard shortcuts, centered "Open a workspace" hero. The workspace list
// lives in the sidebar (FolderList) — no duplicate folder grid here.

import { Show } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";
import { openSidebar } from "../store/uiStore.ts";
import { isCompact } from "../browser/windowSizeClass.ts";
import { defaultNewTerminalWorkerFp } from "../lib/newTerminalTarget.ts";
import { browseHref } from "../routes.ts";
import { BrandMark } from "./machines/BrandMark.tsx";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { platformShortcutLabel } from "../browser/browserPlatform.ts";

export function HomeLanding() {
  const navigate = useNavigate();
  const location = useLocation();

  return (
    <div class="home-landing" data-testid="home-landing">
      <div class="home-landing-head">
        <Show when={isCompact()}>
          <button type="button" class="home-landing-menu"
            data-testid="home-open-sidebar" aria-label="Open sidebar"
            onClick={openSidebar}>☰</button>
          <IconButton
            icon="add"
            label="New terminal"
            title="New terminal"
            data-testid="home-new-terminal"
            onClick={() => {
              const fp = defaultNewTerminalWorkerFp(location.pathname);
              if (fp) navigate(browseHref(fp));
            }}
          />
        </Show>
        <BrandMark size={28} />
        <span class="home-landing-mark">Roost</span>
      </div>

      <p class="home-landing-tagline" data-testid="home-tagline">
        Press <kbd class="home-landing-kbd">{platformShortcutLabel("commandPalette", "⌘K")}</kbd> to open the Command palette ·{" "}
        <kbd class="home-landing-kbd">{platformShortcutLabel("sidebarSearch", "⌘F")}</kbd> to filter the sidebar ·{" "}
        <kbd class="home-landing-kbd">Shift ?</kbd> for shortcuts
      </p>

      <div class="home-landing-empty" data-testid="home-empty">
        <div class="home-landing-empty-icon">
          <BrandMark size={28} />
        </div>
        <div class="home-landing-empty-title">Open a workspace</div>
        <div class="home-landing-empty-sub">
          Select a workspace from the sidebar, or press <kbd class="home-landing-kbd">{platformShortcutLabel("commandPalette", "⌘K")}</kbd> to open the Command palette.
        </div>
      </div>
    </div>
  );
}