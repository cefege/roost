// Settings root. Material 3 navigation rail (left) + top app bar
// (with back action) + scrollable content area. Pane router based on
// /settings/:pane param.

import { useParams, useNavigate } from "@solidjs/router";
import { createMemo, For, Show } from "solid-js";
import { Button, Icon, IconButton } from "./md/primitives.tsx";
import { isCompact } from "../../lib/windowSizeClass.ts";
import { SETTINGS_GROUPS, type SettingsPaneSpec } from "./settingsNavigation.ts";
import { SettingsPane } from "./SettingsPane.tsx";
import { MobileSettingsList } from "./MobileSettingsList.tsx";
import { MobileSettingsDetail } from "./MobileSettingsDetail.tsx";
import "./md/tokens.css";


export function SettingsRoot() {
  const params = useParams<{ pane?: string }>();
  const navigate = useNavigate();

  // Raw URL pane — undefined when at /settings (the list root). The mobile
  // branch reads this so the category list remains the settings home.
  const paneSpec = createMemo((): SettingsPaneSpec | undefined => {
    const paneId = params.pane;
    if (!paneId) return undefined;
    for (const group of SETTINGS_GROUPS) {
      const pane = group.panes.find((candidate) => candidate.id === paneId);
      if (pane) return pane;
    }
    return undefined;
  });
  // Desktop rail/content defaults to the first pane when none is selected,
  // preserving the rail's navigation order.
  const activePane = createMemo((): SettingsPaneSpec => paneSpec() ?? SETTINGS_GROUPS[0]!.panes[0]!);

  return (
    <Show when={isCompact()} fallback={
      <div class="settings-shell">
        <nav class="settings-rail" aria-label="Settings sections">
          <Button variant="text" class="settings-rail__brand" aria-label="Back to app" onClick={() => navigate("/")}>Settings</Button>
          <For each={SETTINGS_GROUPS}>
            {(group) => (
              <div class="settings-rail__group">
                <div class="settings-rail__group-label">{group.label}</div>
                <For each={group.panes}>
                  {(pane) => (
                    <Button
                      variant="text"
                      class="settings-rail__item"
                      data-selected={activePane().id === pane.id ? "true" : "false"}
                      data-testid={`rail-${pane.id}`}
                      onClick={() => navigate(`/settings/${pane.id}`)}
                    >
                      <span class="settings-rail__indicator">
                        <Icon name={pane.icon} filled={activePane().id === pane.id} class="settings-rail__icon" />
                      </span>
                      <span class="settings-rail__label">{pane.label}</span>
                    </Button>
                  )}
                </For>
              </div>
            )}
          </For>
          <div class="settings-rail__spacer" />
        </nav>

        <main class="settings-main">
          <header class="settings-topbar">
            <IconButton
              class="settings-topbar__back"
              icon="arrow_back"
              label="Back to app"
              data-testid="settings-back"
              onClick={() => navigate("/")}
            />
            <h1 class="settings-topbar__title">{activePane().title}</h1>
          </header>
          <div class="settings-content">
            <div class="settings-content__inner">
              <SettingsPane id={activePane().id} />
            </div>
          </div>
        </main>
      </div>
    }>
      <Show when={paneSpec()} fallback={<MobileSettingsList />}>
        {(spec) => <MobileSettingsDetail spec={spec()} />}
      </Show>
    </Show>
  );
}

