// Route-stable primary sidebar owner.
// AppShell mounts this composition in the desktop aside and compact drawer.
// It retains both native-scroll projections while a persisted selector controls
// their visible and interactive state.

import { Show } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { isCompact } from "../../lib/windowSizeClass.ts";
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

export function SidebarRoot() {
  const navigate = useNavigate();
  const sidebarInteractive = () => isCompact() ? uiStore.sidebarOpen : !uiStore.sidebarCollapsed;
  const spacesSelected = () => uiStore.sidebarView === "spaces";
  const agentsSelected = () => uiStore.sidebarView === "agents";
  const spacesActive = () => sidebarInteractive() && spacesSelected();
  const agentsActive = () => sidebarInteractive() && agentsSelected();

  function activateSpaces(): void {
    if (isCompact()) {
      if (!uiStore.sidebarOpen) openSidebar();
    } else if (uiStore.sidebarCollapsed) {
      toggleSidebarCollapsed();
    }
    setSidebarView("spaces");
  }

  return (
    <div class="workbench-sidebar-root" data-testid="sidebar-root">
      <div class="workbench-sidebar-selector" data-testid="sidebar-selector" role="group" aria-label="Sidebar view">
        <Button
          id="sidebar-view-spaces"
          class="workbench-sidebar-selector__control"
          data-selected={uiStore.sidebarView === "spaces" ? "true" : "false"}
          data-testid="sidebar-view-spaces"
          size="sm"
          variant="ghost"
          aria-pressed={uiStore.sidebarView === "spaces"}
          onClick={() => setSidebarView("spaces")}
        >
          Spaces
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

      <div class="workbench-sidebar-panels">
        <div
          class="workbench-sidebar-panel workbench-sidebar-panel--spaces"
          data-active={spacesSelected() ? "true" : "false"}
          data-testid="sidebar-spaces"
          inert={!spacesActive() ? true : undefined}
          aria-hidden={spacesSelected() ? undefined : "true"}
        >
          <AllView active={spacesActive()} onActivate={activateSpaces} />
        </div>
        <div
          class="workbench-sidebar-panel workbench-sidebar-panel--agents"
          data-active={agentsSelected() ? "true" : "false"}
          data-testid="sidebar-agents-section"
          inert={!agentsActive() ? true : undefined}
          aria-hidden={agentsSelected() ? undefined : "true"}
        >
          <SidebarAgents />
        </div>
      </div>

      <Show when={isCompact()}>
        <footer class="workbench-sidebar-footer">
          <IconButton
            icon="settings"
            label="Settings"
            onClick={() => navigate(settingsPaneHref("devices"))}
            data-testid="brand-row-settings"
          />
          <IconButton
            icon="close"
            label="Close sidebar"
            onClick={closeSidebar}
            data-testid="brand-row-collapse"
          />
        </footer>
      </Show>
    </div>
  );
}
