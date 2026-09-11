// Settings root owns the URL-derived desktop workbench rail and compact route split.
// Desktop keeps a static Settings rail with the selected pane in editor content.
// Mobile preserves its list/detail transition model through the dedicated components.

import { useParams, useNavigate } from "@solidjs/router";
import { createMemo, For, Show } from "solid-js";
import { Icon, IconButton, ListRow } from "./md/primitives.tsx";
import { isCompact } from "../../lib/windowSizeClass.ts";
import { SETTINGS_GROUPS, type SettingsPaneSpec } from "./settingsNavigation.ts";
import { SettingsPane } from "./SettingsPane.tsx";
import { MobileSettingsList } from "./MobileSettingsList.tsx";
import { MobileSettingsDetail } from "./MobileSettingsDetail.tsx";
import { settingsPaneHref } from "../../routes.ts";


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
        <aside class="settings-rail" aria-label="Settings sections">
          <h1 class="settings-rail__title">Settings</h1>
          <For each={SETTINGS_GROUPS}>
            {(group) => (
              <section class="settings-rail__group">
                <h2 class="settings-rail__group-label">{group.label}</h2>
                <For each={group.panes}>
                  {(pane) => (
                    <ListRow
                      class="settings-rail__item"
                      href={settingsPaneHref(pane.id)}
                      selected={activePane().id === pane.id}
                      ariaCurrent={activePane().id === pane.id ? "page" : undefined}
                      testId={`rail-${pane.id}`}
                      leading={<Icon name={pane.icon} />}
                      headline={pane.label}
                    />
                  )}
                </For>
              </section>
            )}
          </For>
        </aside>

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

