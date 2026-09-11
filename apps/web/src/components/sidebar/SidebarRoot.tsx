// Route-stable primary sidebar owner.
// AppShell mounts this composition for desktop and the compact navigation drawer.
// Expanded desktop keeps Spaces and Agents independently scrollable without
// replacing the workspace or terminal context that surrounding routes retain.

import { createMemo, Show } from "solid-js";
import { isCompact } from "../../lib/windowSizeClass.ts";
import { uiStore } from "../../store/uiStore.ts";
import { AllView } from "./AllView.tsx";
import { SidebarAgents } from "./SidebarAgents.tsx";
import { SidebarSectionResizer } from "./SidebarSectionResizer.tsx";

export function SidebarRoot() {
  let sidebarRoot: HTMLElement | undefined;
  const splitSidebarVisible = createMemo(
    () => !isCompact() && !uiStore.sidebarCollapsed,
  );

  return (
    <div
      ref={(element) => { sidebarRoot = element; }}
      class="workbench-sidebar-root"
      data-testid="sidebar-root"
      data-split={splitSidebarVisible() ? "true" : "false"}
      style={{ "--workbench-sidebar-split-ratio": String(uiStore.sidebarSplitRatio) }}
    >
      <section
        class="workbench-sidebar-section workbench-sidebar-section--spaces"
        data-testid="sidebar-spaces"
        aria-label="Spaces"
      >
        <AllView />
      </section>
      <Show when={splitSidebarVisible()}>
        <SidebarSectionResizer container={() => sidebarRoot} />
        <div
          class="workbench-sidebar-section workbench-sidebar-section--agents"
          data-testid="sidebar-agents-section"
        >
          <SidebarAgents />
        </div>
      </Show>
    </div>
  );
}
