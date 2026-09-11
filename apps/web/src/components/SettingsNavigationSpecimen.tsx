// SettingsNavigationSpecimen renders the desktop Settings rail reference for /design.
// It reuses SettingsRoot's classes, navigation order, and ListRow link anatomy.
// DesignGallery mounts it for visual review; SettingsRoot remains the live route owner.
// It depends on workbench tokens and the shared Settings primitives.

import type { Component } from "solid-js";
import { For } from "solid-js";
import { settingsPaneHref } from "../routes.ts";
import { SETTINGS_GROUPS } from "./Settings/settingsNavigation.ts";
import { Icon, ListRow } from "./Settings/md/primitives.tsx";

export const SettingsNavigationSpecimen: Component = () => (
  <div class="settings-shell" style={{ height: "calc(var(--md-space-9) * 6)" }}>
    <aside class="settings-rail" aria-label="Settings sections">
      <h1 class="settings-rail__title">Settings</h1>
      <For each={SETTINGS_GROUPS}>
        {(group) => (
          <section class="settings-rail__group">
            <h2 class="settings-rail__group-label">{group.label}</h2>
            <For each={group.panes}>
              {(pane) => {
                const selected = pane.id === "machines";
                return (
                  <ListRow
                    class="settings-rail__item"
                    href={settingsPaneHref(pane.id)}
                    selected={selected}
                    ariaCurrent={selected ? "page" : undefined}
                    leading={<Icon name={pane.icon} />}
                    headline={pane.label}
                  />
                );
              }}
            </For>
          </section>
        )}
      </For>
    </aside>
    <main class="settings-main">
      <header class="settings-topbar">
        <h2 class="settings-topbar__title">Machines</h2>
      </header>
      <div class="settings-content" />
    </main>
  </div>
);
