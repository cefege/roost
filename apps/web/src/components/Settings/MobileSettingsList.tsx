// Compact settings category list. SettingsRoot renders it only at the /settings list route.
// Selection keeps the existing view-transition push behavior and leaves the shared pane mapping untouched.
// Its own header replaces AppShell's generic compact title bar for Settings.

import { For } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { Icon, IconButton, List, ListRow } from "./md/primitives.tsx";
import { SETTINGS_GROUPS } from "./settingsNavigation.ts";
import { withViewTransition } from "../../lib/viewTransition.ts";
import { settingsPaneHref } from "../../routes.ts";

export function MobileSettingsList() {
  const navigate = useNavigate();

  function openSettingsPane(paneId: string) {
    document.documentElement.style.setProperty("--settings-nav-dir", "1");
    withViewTransition(() => navigate(settingsPaneHref(paneId)));
  }

  return (
    <div class="settings-mobile__main">
      <header class="settings-topbar">
        <IconButton
          class="settings-topbar__back"
          icon="arrow_back"
          label="Back to app"
          data-testid="settings-back"
          onClick={() => navigate("/")}
        />
        <h1 class="settings-topbar__title">Settings</h1>
      </header>
      <List class="settings-mobile__list">
        <For each={SETTINGS_GROUPS}>
          {(group) => (
            <div class="settings-mobile__group">
              <div class="settings-mobile__group-label">{group.label}</div>
              <For each={group.panes}>
                {(pane) => (
                  <ListRow
                    class="settings-mobile__row"
                    testId={`settings-list-${pane.id}`}
                    onClick={() => openSettingsPane(pane.id)}
                    leading={<Icon name={pane.icon} class="settings-mobile__icon" />}
                    headline={<span class="settings-mobile__label">{pane.label}</span>}
                    trailing={<Icon name="chevron_right" class="settings-mobile__chev" />}
                  />
                )}
              </For>
            </div>
          )}
        </For>
      </List>
    </div>
  );
}
