// Home landing ("hello" pane) rendered at "/" when workers exist but no
// session is auto-opened. Chrome-new-tab-page-style empty state: brand row,
// keyboard shortcuts, centered "Open a workspace" hero. The workspace list
// lives in the sidebar (FolderList) — no duplicate folder grid here.

import { Show } from "solid-js";
import { openSidebar } from "../store/uiStore.ts";
import { isCompact } from "../lib/windowSizeClass.ts";
import { BrandMark } from "./BrandMark.tsx";
import { FlatNewTerminal } from "./sidebar/FlatNewTerminal.tsx";
import { NotificationBellTrigger } from "./NotificationBell.tsx";

export function HomeLanding() {
  return (
    <div class="home-landing" data-testid="home-landing">
      <div class="home-landing-head">
        <Show when={isCompact()}>
          <button type="button" class="home-landing-menu"
            data-testid="home-open-sidebar" aria-label="Open sidebar"
            onClick={openSidebar}>☰</button>
        </Show>
        <BrandMark size={28} />
        <span class="home-landing-mark">Roost</span>
        <Show when={isCompact()}>
          <NotificationBellTrigger style={{ "flex-shrink": 0 }} />
        </Show>
        <FlatNewTerminal />
      </div>

      <p class="home-landing-tagline" data-testid="home-tagline">
        Press <kbd class="home-landing-kbd">⌘K</kbd> to open a terminal ·{" "}
        <kbd class="home-landing-kbd">⌘F</kbd> to filter the sidebar ·{" "}
        <kbd class="home-landing-kbd">Shift ?</kbd> for shortcuts
      </p>

      <div class="home-landing-empty" data-testid="home-empty">
        <div class="home-landing-empty-icon">
          <BrandMark size={28} />
        </div>
        <div class="home-landing-empty-title">Open a workspace</div>
        <div class="home-landing-empty-sub">
          Select a workspace from the sidebar, or press <kbd class="home-landing-kbd">⌘K</kbd> to open a new terminal.
        </div>
      </div>
    </div>
  );
}