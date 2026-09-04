// Settings root. Material 3 navigation rail (left) + top app bar
// (with back action) + scrollable content area. Pane router based on
// /settings/:pane param.
// Managed coordinators replace the self-hosted scope, connection, and pairing
// surfaces with one Account pane.

import { useParams, useNavigate } from "@solidjs/router";
import { createEffect, createMemo, Switch, Match, For, Show } from "solid-js";
import { MachinesPane } from "./MachinesPane.tsx";
import { McpPane } from "./McpPane.tsx";
import { ThemePane } from "./ThemePane.tsx";
import { AgentLauncherPane } from "./AgentLauncherPane.tsx";
import { AuditLogPane } from "./AuditLogPane.tsx";
import { MetricsPane } from "./MetricsPane.tsx";
import { AttachmentsPane } from "./AttachmentsPane.tsx";
import { DevicesPane } from "./DevicesPane.tsx";
import { AccountPane } from "./AccountPane.tsx";
import { TranscriptionPane } from "./TranscriptionPane.tsx";
import { TerminalPane } from "./TerminalPane.tsx";
import { ConnectionPane } from "./ConnectionPane.tsx";
import { NotificationsPane } from "./NotificationsPane.tsx";
import { OrganizationPane } from "./OrganizationPane.tsx";
import { DashboardPane } from "./DashboardPane.tsx";
import { Icon } from "./md/primitives.tsx";
import { isCompact } from "../../lib/windowSizeClass.ts";
import { withViewTransition } from "../../lib/viewTransition.ts";
import { rootStore } from "../../store/root.ts";
import { settingsPaneHref } from "../../routes.ts";
import {
  isHiddenManagedSettingsPane,
  resolveSettingsPaneForMode,
  settingsGroupsForMode,
  type SettingsPaneSpec,
  type SettingsRailGroup,
} from "./settingsNavigation.ts";
import "./md/tokens.css";

// Self-hosted Devices merges phone pairing and browser approval. Managed
// coordinators instead route every legacy scope/device URL to Account.
function visibleSettingsGroups(): readonly SettingsRailGroup[] {
  return settingsGroupsForMode(rootStore.coord_identity?.saas_mode === true);
}
/** Mobile push/pop: detail slides in from the right (dir 1 = forward),
 *  back pops it out to the right (dir -1). Direction-aware slide via
 *  --settings-nav-dir; withViewTransition handles feature-detect + reduced
 *  motion (instant navigate fallback). Desktop rail is NOT routed through
 *  this — only the two mobile navigate calls use it. */
function slideNavigate(
  navigate: (path: string, opts?: { replace?: boolean }) => void,
  path: string,
  opts: { replace?: boolean } | undefined,
  dir: 1 | -1,
): void {
  document.documentElement.style.setProperty("--settings-nav-dir", String(dir));
  withViewTransition(() => navigate(path, opts));
}
// Shared pane router — used by both desktop main and mobile detail so the
// two surfaces render identical pane content with zero duplication.
function SettingsPane(props: { id: string }) {
  const id = () => resolveSettingsPaneForMode(
    props.id,
    rootStore.coord_identity?.saas_mode === true,
  );
  return (
    <Switch>
      <Match when={id() === "account"}><AccountPane /></Match>
      <Match when={id() === "machines"}><MachinesPane /></Match>
      <Match when={id() === "organization"}><OrganizationPane /></Match>
      <Match when={id() === "dashboard"}><DashboardPane /></Match>
      <Match when={id() === "connection"}><ConnectionPane /></Match>
      <Match when={id() === "devices"}><DevicesPane /></Match>
      <Match when={id() === "launcher"}><AgentLauncherPane /></Match>
      <Match when={id() === "mcp"}><McpPane /></Match>
      <Match when={id() === "voice"}><TranscriptionPane /></Match>
      <Match when={id() === "terminal"}><TerminalPane /></Match>
      <Match when={id() === "notifications"}><NotificationsPane /></Match>
      <Match when={id() === "attachments"}><AttachmentsPane /></Match>
      <Match when={id() === "theme"}><ThemePane /></Match>
      <Match when={id() === "audit"}><AuditLogPane /></Match>
      <Match when={id() === "metrics"}><MetricsPane /></Match>
    </Switch>
  );
}

export function SettingsRoot() {
  const params = useParams<{ pane?: string }>();
  const navigate = useNavigate();
  createEffect(() => {
    if (
      rootStore.coord_identity?.saas_mode === true
      && isHiddenManagedSettingsPane(params.pane)
    ) {
      navigate(settingsPaneHref("account"), { replace: true });
    }
  });

  // Raw URL pane — undefined when at /settings (the list root). The mobile
  // branch reads this so the category list remains the settings home.
  const paneSpec = createMemo((): SettingsPaneSpec | undefined => {
    const paneId = params.pane;
    if (!paneId) return undefined;
    for (const group of visibleSettingsGroups()) {
      const pane = group.panes.find((candidate) => candidate.id === paneId);
      if (pane) return pane;
    }
    return undefined;
  });
  // Desktop rail/content defaults to the first visible pane when none is
  // selected, preserving the profile's navigation order.
  const activePane = createMemo((): SettingsPaneSpec => paneSpec() ?? visibleSettingsGroups()[0]!.panes[0]!);

  return (
    <Show when={isCompact()} fallback={
      <div class="settings-shell">
        <nav class="settings-rail" aria-label="Settings sections">
          <button type="button" class="settings-rail__brand" aria-label="Back to app" onClick={() => navigate("/")}>Settings</button>
          <For each={visibleSettingsGroups()}>
            {(group) => (
              <div class="settings-rail__group">
                <div class="settings-rail__group-label">{group.label}</div>
                <For each={group.panes}>
                  {(pane) => (
                    <button
                      type="button"
                      class="settings-rail__item"
                      data-selected={activePane().id === pane.id ? "true" : "false"}
                      data-testid={`rail-${pane.id}`}
                      onClick={() => navigate(`/settings/${pane.id}`)}
                    >
                      <span class="settings-rail__indicator">
                        <Icon name={pane.icon} filled={activePane().id === pane.id} class="settings-rail__icon" />
                      </span>
                      <span class="settings-rail__label">{pane.label}</span>
                    </button>
                  )}
                </For>
              </div>
            )}
          </For>
          <div class="settings-rail__spacer" />
        </nav>

        <main class="settings-main">
          <header class="settings-topbar">
            <button
              type="button"
              class="settings-topbar__back"
              aria-label="Back to app"
              data-testid="settings-back"
              onClick={() => navigate("/")}
            >
              <Icon name="arrow_back" />
            </button>
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

// ── Mobile settings LIST (settings home — /settings) ───────────────
function MobileSettingsList() {
  const navigate = useNavigate();
  return (
    <div class="settings-mobile__main">
      <header class="settings-topbar">
        <button
          type="button"
          class="settings-topbar__back"
          aria-label="Back to app"
          data-testid="settings-back"
          onClick={() => navigate("/")}
        >
          <Icon name="arrow_back" />
        </button>
        <h1 class="settings-topbar__title">Settings</h1>

      </header>
      <div class="settings-mobile__list">
        <For each={visibleSettingsGroups()}>
          {(group) => (
            <div class="settings-mobile__group">
              <div class="settings-mobile__group-label">{group.label}</div>
              <For each={group.panes}>
                {(pane) => (
                  <button
                    type="button"
                    class="settings-mobile__row"
                    attr:data-testid={`settings-list-${pane.id}`}
                    onClick={() => slideNavigate(navigate, `/settings/${pane.id}`, undefined, 1)}
                  >
                    <Icon name={pane.icon} class="settings-mobile__icon" />
                    <span class="settings-mobile__label">{pane.label}</span>
                    <Icon name="chevron_right" class="settings-mobile__chev" />
                  </button>
                )}
              </For>
            </div>
          )}
        </For>
      </div>
    </div>
  );
}

// ── Mobile settings DETAIL (/settings/:pane) ──────────────────────
function MobileSettingsDetail(props: { spec: SettingsPaneSpec }) {
  const navigate = useNavigate();
  return (
    <div class="settings-mobile__main">
      <header class="settings-topbar">
        <button
          type="button"
          class="settings-topbar__back"
          aria-label="Back to settings"
          data-testid="settings-detail-back"
          onClick={() => slideNavigate(navigate, "/settings", { replace: true }, -1)}
        >
          <Icon name="arrow_back" />
        </button>
        <h1 class="settings-topbar__title">{props.spec.title}</h1>

      </header>
      <div class="settings-content">
        <div class="settings-content__inner">
          <SettingsPane id={props.spec.id} />
        </div>
      </div>
    </div>
  );
}
