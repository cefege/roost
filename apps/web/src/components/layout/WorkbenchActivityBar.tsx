// Desktop activity rail. Owns fixed Roost destinations and URL-derived active state.
// AppShell mounts it beside the existing SidebarRoot; it never owns a view registry.
// The active Sessions destination toggles the existing primary sidebar collapse action.

import { A, useLocation } from "@solidjs/router";
import { Icon } from "../Settings/md/primitives.tsx";
import { ROUTES, settingsPaneHref } from "../../routes.ts";

function isSessionsRoute(pathname: string): boolean {
  return pathname === ROUTES.ROOT
    || pathname.startsWith("/s/")
    || pathname.startsWith("/t/")
    || pathname.startsWith("/w/");
}

interface WorkbenchActivityBarProps {
  onToggleSidebar: () => void;
}

export function WorkbenchActivityBar(props: WorkbenchActivityBarProps) {
  const location = useLocation();
  const sessionsActive = () => isSessionsRoute(location.pathname);
  const searchActive = () => location.pathname.startsWith(ROUTES.SEARCH);
  const filesActive = () => location.pathname.startsWith("/file/") || location.pathname.startsWith("/browse");
  const settingsActive = () => location.pathname.startsWith("/settings");
  const helpActive = () => location.pathname.startsWith(ROUTES.HELP);

  function toggleActiveSessions(event: MouseEvent) {
    if (!sessionsActive() || event.defaultPrevented || event.button !== 0
      || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    props.onToggleSidebar();
  }

  return (
    <nav class="workbench-activity-bar" aria-label="Workbench navigation">
      <div class="workbench-activity-bar__group">
        <A
          id="workbench-activity-sessions"
          class="workbench-activity-bar__item"
          data-active={sessionsActive() ? "true" : "false"}
          data-testid="workbench-activity-sessions"
          href={ROUTES.ROOT}
          aria-label="Sessions"
          aria-current={sessionsActive() ? "page" : undefined}
          title="Sessions"
          onClick={toggleActiveSessions}
        >
          <Icon name="terminal" filled={sessionsActive()} />
          <span class="workbench-activity-bar__label">Sessions</span>
        </A>
        <A
          class="workbench-activity-bar__item"
          data-active={searchActive() ? "true" : "false"}
          data-testid="workbench-activity-search"
          href={ROUTES.SEARCH}
          aria-label="Search"
          aria-current={searchActive() ? "page" : undefined}
          title="Search"
        >
          <Icon name="search" filled={searchActive()} />
          <span class="workbench-activity-bar__label">Search</span>
        </A>
        <A
          class="workbench-activity-bar__item"
          data-active={filesActive() ? "true" : "false"}
          data-testid="workbench-activity-files"
          href={ROUTES.BROWSE_ROOT}
          aria-label="Files"
          aria-current={filesActive() ? "page" : undefined}
          title="Files"
        >
          <Icon name="folder_open" filled={filesActive()} />
          <span class="workbench-activity-bar__label">Files</span>
        </A>
      </div>
      <div class="workbench-activity-bar__group workbench-activity-bar__group--bottom">
        <A
          class="workbench-activity-bar__item"
          data-active={settingsActive() ? "true" : "false"}
          data-testid="workbench-activity-settings"
          href={settingsPaneHref("machines")}
          aria-label="Settings"
          aria-current={settingsActive() ? "page" : undefined}
          title="Settings"
        >
          <Icon name="settings" filled={settingsActive()} />
          <span class="workbench-activity-bar__label">Settings</span>
        </A>
        <A
          class="workbench-activity-bar__item"
          data-active={helpActive() ? "true" : "false"}
          data-testid="workbench-activity-help"
          href={ROUTES.HELP}
          aria-label="Help"
          aria-current={helpActive() ? "page" : undefined}
          title="Help"
        >
          <Icon name="help" filled={helpActive()} />
          <span class="workbench-activity-bar__label">Help</span>
        </A>
      </div>
    </nav>
  );
}
