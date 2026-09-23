// Folders sidebar panel.
// SidebarRoot owns the shared filter input and debounce; FolderList remains
// mounted while it switches between folder and filtered-session projections.
// FolderList retains selection and keyboard-cursor ownership.

import { createMemo, Show } from "solid-js";
import { rootStore } from "../../store/root.ts";
import { FolderList } from "./FolderList.tsx";
import { SidebarEmptyState } from "./SidebarEmptyState.tsx";

interface AllViewProps {
  active: boolean;
  query: string;
}

export function AllView(props: AllViewProps) {
  const noMachines = createMemo(() => Object.keys(rootStore.workers).length === 0);
  const folderListActive = () => props.active && !noMachines();

  return (
    <div class="df-all-view workbench-sidebar-content" data-testid="all-view">
      <Show when={!noMachines()} fallback={<SidebarEmptyState kind="no-machines" />}>
        <div
          class="workbench-sidebar-folder-list"
          data-active={folderListActive() ? "true" : "false"}
          inert={!folderListActive() ? true : undefined}
          aria-hidden={folderListActive() ? undefined : "true"}
        >
          <FolderList active={folderListActive()} query={props.query} />
        </div>
      </Show>
    </div>
  );
}
