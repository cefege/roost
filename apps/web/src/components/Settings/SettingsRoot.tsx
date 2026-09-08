// Settings root. Material 3 navigation rail (left) + top app bar
// (with back action) + scrollable content area. Pane router based on
// /settings/:pane param.

import { useParams, useNavigate } from "@solidjs/router";
import { createMemo, Switch, Match, For, Show } from "solid-js";
import { MachinesPane } from "./MachinesPane.tsx";
import { McpPane } from "./McpPane.tsx";
import { ThemePane } from "./ThemePane.tsx";
import { AgentLauncherPane } from "./AgentLauncherPane.tsx";
import { AuditLogPane } from "./AuditLogPane.tsx";
import { MetricsPane } from "./MetricsPane.tsx";
import { AttachmentsPane } from "./AttachmentsPane.tsx";
import { DevicesPane } from "./DevicesPane.tsx";
import { TranscriptionPane } from "./TranscriptionPane.tsx";
import { TerminalPane } from "./TerminalPane.tsx";
import { ConnectionPane } from "./ConnectionPane.tsx";
import { NotificationsPane } from "./NotificationsPane.tsx";
import { OrganizationPane } from "./OrganizationPane.tsx";
import { DashboardPane } from "./DashboardPane.tsx";
import { Icon } from "./md/primitives.tsx";
import { isCompact } from "../../lib/windowSizeClass.ts";
import { withViewTransition } from "../../lib/viewTransition.ts";
import { SETTINGS_GROUPS, type SettingsPaneSpec } from "./settingsNavigation.ts";
import "./md/tokens.css";

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
  return (
    <Switch>
      <Match when={props.id === "machines"}><MachinesPane /></Match>
      <Match when={props.id === "organization"}><OrganizationPane /></Match>
      <Match when={props.id === "dashboard"}><DashboardPane /></Match>
      <Match when={props.id === "connection"}><ConnectionPane /></Match>
      <Match when={props.id === "devices"}><DevicesPane /></Match>
      <Match when={props.id === "launcher"}><AgentLauncherPane /></Match>
      <Match when={props.id === "mcp"}><McpPane /></Match>
      <Match when={props.id === "voice"}><TranscriptionPane /></Match>
      <Match when={props.id === "terminal"}><TerminalPane /></Match>
      <Match when={props.id === "notifications"}><NotificationsPane /></Match>
      <Match when={props.id === "attachments"}><AttachmentsPane /></Match>
      <Match when={props.id === "theme"}><ThemePane /></Match>
      <Match when={props.id === "audit"}><AuditLogPane /></Match>
      <Match when={props.id === "metrics"}><MetricsPane /></Match>
    </Switch>
  );
}

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
          <button type="button" class="settings-rail__brand" aria-label="Back to app" onClick={() => navigate("/")}>Settings</button>
          <For each={SETTINGS_GROUPS}>
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
        <For each={SETTINGS_GROUPS}>
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
