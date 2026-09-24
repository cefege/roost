// Route-stable primary sidebar owner.
// AppShell mounts this composition in the desktop aside and compact drawer.
// It retains both native-scroll projections while a persisted selector controls
// their visible and interactive state.

import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { matchesPlatformShortcut } from "../../browser/browserPlatform.ts";
import { terminalOwnsKeyboard } from "../../lib/keyboardShortcuts.ts";
import { isCompact } from "../../browser/windowSizeClass.ts";
import { settingsPaneHref } from "../../routes.ts";
import {
  closeSidebar,
  openSidebar,
  setSidebarView,
  toggleSidebarCollapsed,
  uiStore,
} from "../../store/uiStore.ts";
import { Button } from "../Settings/md/Button.tsx";
import { IconButton } from "../Settings/md/IconButton.tsx";
import { AllView } from "./AllView.tsx";
import { SidebarAgents } from "./SidebarAgents.tsx";
import { SidebarNewTerminal } from "./SidebarNewTerminal.tsx";
import { SidebarSearch } from "./SidebarSearch.tsx";

const SEARCH_DEBOUNCE_MS = 120;

export function SidebarRoot() {
  const navigate = useNavigate();
  const [query, setQuery] = createSignal("");
  const [debouncedQuery, setDebouncedQuery] = createSignal("");
  let debounceTimer: number | undefined;
  let focusFrame: number | undefined;
  let searchRef: HTMLInputElement | undefined;

  function onQueryChange(next: string): void {
    setQuery(next);
    clearTimeout(debounceTimer);
    if (next.trim().length === 0) {
      setDebouncedQuery("");
      return;
    }
    debounceTimer = window.setTimeout(() => setDebouncedQuery(next), SEARCH_DEBOUNCE_MS);
  }

  function onGlobalKeyDown(event: KeyboardEvent): void {
    if (event.defaultPrevented || terminalOwnsKeyboard()) return;
    if (!matchesPlatformShortcut(event, "sidebarSearch")) return;
    event.preventDefault();
    if (isCompact()) {
      if (!uiStore.sidebarOpen) openSidebar();
    } else if (uiStore.sidebarCollapsed) {
      toggleSidebarCollapsed();
    }
    if (focusFrame !== undefined) cancelAnimationFrame(focusFrame);
    focusFrame = requestAnimationFrame(() => {
      focusFrame = undefined;
      searchRef?.focus();
    });
  }

  onMount(() => document.addEventListener("keydown", onGlobalKeyDown));
  onCleanup(() => {
    clearTimeout(debounceTimer);
    if (focusFrame !== undefined) cancelAnimationFrame(focusFrame);
    document.removeEventListener("keydown", onGlobalKeyDown);
  });

  const sidebarInteractive = () => isCompact() ? uiStore.sidebarOpen : !uiStore.sidebarCollapsed;
  const foldersSelected = () => uiStore.sidebarView === "folders";
  const agentsSelected = () => uiStore.sidebarView === "agents";
  const foldersActive = () => sidebarInteractive() && foldersSelected();
  const agentsActive = () => sidebarInteractive() && agentsSelected();

  return (
    <div class="workbench-sidebar-root" data-testid="sidebar-root">
      <div>
        <div class="workbench-sidebar-header">
          <Show when={isCompact()}>
            <IconButton
              icon="close"
              label="Close sidebar"
              onClick={closeSidebar}
              data-testid="brand-row-collapse"
            />
          </Show>
          <div class="workbench-sidebar-selector" data-testid="sidebar-selector" role="group" aria-label="Sidebar view">
            <Button
              id="sidebar-view-folders"
              class="workbench-sidebar-selector__control"
              data-selected={uiStore.sidebarView === "folders" ? "true" : "false"}
              data-testid="sidebar-view-folders"
              size="sm"
              variant="ghost"
              aria-pressed={uiStore.sidebarView === "folders"}
              onClick={() => setSidebarView("folders")}
            >
              Folders
            </Button>
            <Button
              id="sidebar-view-agents"
              class="workbench-sidebar-selector__control"
              data-selected={uiStore.sidebarView === "agents" ? "true" : "false"}
              data-testid="sidebar-view-agents"
              size="sm"
              variant="ghost"
              aria-pressed={uiStore.sidebarView === "agents"}
              onClick={() => setSidebarView("agents")}
            >
              Agents
            </Button>
          </div>
          <Show when={isCompact()}>
            <IconButton
              icon="settings"
              label="Settings"
              onClick={() => navigate(settingsPaneHref("devices"))}
              data-testid="brand-row-settings"
            />
          </Show>
        </div>
        <SidebarSearch
          query={query()}
          onChange={onQueryChange}
          inputRef={(element) => { searchRef = element; }}
          placeholder={foldersSelected() ? "Filter folders…" : "Filter agents…"}
        />
      </div>

      <div class="workbench-sidebar-panels">
        <div
          class="workbench-sidebar-panel workbench-sidebar-panel--folders"
          data-active={foldersSelected() ? "true" : "false"}
          data-testid="sidebar-folders"
          inert={!foldersActive() ? true : undefined}
          aria-hidden={foldersSelected() ? undefined : "true"}
        >
          <AllView active={foldersActive()} query={debouncedQuery()} />
        </div>
        <div
          class="workbench-sidebar-panel workbench-sidebar-panel--agents"
          data-active={agentsSelected() ? "true" : "false"}
          data-testid="sidebar-agents-section"
          inert={!agentsActive() ? true : undefined}
          aria-hidden={agentsSelected() ? undefined : "true"}
        >
          <SidebarAgents query={debouncedQuery()} />
        </div>
      </div>

      <SidebarNewTerminal />
    </div>
  );
}
