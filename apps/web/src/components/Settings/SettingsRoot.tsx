// Settings root. Material 3 navigation rail (left) + top app bar
// (with back action) + scrollable content area. Pane router based on
// /settings/:pane param.
// Panes: machines | mcp | permissions | webhooks | theme | audit |
//        metrics | attachments | browser-pairing.

import { useParams, useNavigate } from "@solidjs/router";
import { createMemo, Switch, Match, For, Show } from "solid-js";
import { MachinesPane } from "./MachinesPane.tsx";
import { McpPane } from "./McpPane.tsx";
import { PermissionsPane } from "./PermissionsPane.tsx";
import { WebhooksPane } from "./WebhooksPane.tsx";
import { ThemePane } from "./ThemePane.tsx";
import { AgentLauncherPane } from "./AgentLauncherPane.tsx";
import { AuditLogPane } from "./AuditLogPane.tsx";
import { MetricsPane } from "./MetricsPane.tsx";
import { AttachmentsPane } from "./AttachmentsPane.tsx";
import { DevicesPane } from "./DevicesPane.tsx";
import { TranscriptionPane } from "./TranscriptionPane.tsx";
import { TerminalPane } from "./TerminalPane.tsx";
import { NotificationsPane } from "./NotificationsPane.tsx";
import { ConnectionPane } from "./ConnectionPane.tsx";
import { Icon } from "./md/primitives.tsx";
import { isCompact } from "../../lib/windowSizeClass.ts";
import "./md/tokens.css";

// The "Devices" pane (DevicesPane) merges phone-pairing (QR) + browser
// approval (Onboarding) — Onboarding is also rendered at /pair for the
// cross-browser entry point. Rail items are grouped into labeled sections.
interface PaneSpec {
  id: string;
  label: string;
  icon: string;
  title: string;
}
interface RailGroup {
  label: string;
  panes: PaneSpec[];
}
const GROUPS: RailGroup[] = [
  { label: "Network", panes: [
    { id: "machines",   label: "Machines",   icon: "desktop_mac", title: "Machines" },
    { id: "connection", label: "Connection", icon: "lan",         title: "Connection" },
    { id: "devices",    label: "Devices",    icon: "devices",     title: "Devices" },
  ] },
  { label: "Agents", panes: [
    { id: "launcher",    label: "Launcher", icon: "rocket_launch", title: "Default agent" },
    { id: "mcp",         label: "MCP",      icon: "extension", title: "MCP relays" },
    { id: "permissions", label: "Access",   icon: "lock",      title: "Permissions" },
    { id: "webhooks",    label: "Webhooks", icon: "webhook",   title: "Webhooks" },
  ] },
  { label: "Interface", panes: [
    { id: "terminal",       label: "Terminal",       icon: "terminal",       title: "Terminal" },
    { id: "notifications",  label: "Notifications",  icon: "notifications",  title: "Notifications" },
    { id: "voice",    label: "Voice",    icon: "mic",      title: "Voice dictation" },
    { id: "theme",    label: "Theme",    icon: "palette",  title: "Theme" },
  ] },
  { label: "System", panes: [
    { id: "attachments", label: "Files",   icon: "folder_open", title: "Attachments" },
    { id: "audit",       label: "Audit",   icon: "history",     title: "Audit log" },
    { id: "metrics",     label: "Metrics", icon: "monitoring",  title: "Metrics" },
  ] },
];
const ALL_PANES: PaneSpec[] = GROUPS.flatMap((g) => g.panes);
const REDUCED = (): boolean =>
  typeof window !== "undefined" &&
  window.matchMedia?.("(prefers-reduced-motion: reduce)").matches === true;

/** Mobile push/pop: detail slides in from the right (dir 1 = forward),
 *  back pops it out to the right (dir -1). View Transitions API where
 *  available; instant navigate fallback otherwise. Desktop rail is NOT
 *  routed through this — only the two mobile navigate calls use it. */
function slideNavigate(
  navigate: (path: string, opts?: { replace?: boolean }) => void,
  path: string,
  opts: { replace?: boolean } | undefined,
  dir: 1 | -1,
): void {
  const sv = (document as Document & {
    startViewTransition?: (cb: () => void) => unknown;
  }).startViewTransition;
  if (!sv || REDUCED()) { navigate(path, opts); return; }
  document.documentElement.style.setProperty("--settings-nav-dir", String(dir));
  sv.call(document, () => navigate(path, opts));
}
// Shared pane router — used by both desktop main and mobile detail so the
// two surfaces render identical pane content with zero duplication.
function SettingsPane(props: { id: string }) {
  const id = () => props.id;
  return (
    <Switch>
      <Match when={id() === "machines"}><MachinesPane /></Match>
      <Match when={id() === "connection"}><ConnectionPane /></Match>
      <Match when={id() === "devices"}><DevicesPane /></Match>
      <Match when={id() === "launcher"}><AgentLauncherPane /></Match>
      <Match when={id() === "mcp"}><McpPane /></Match>
      <Match when={id() === "permissions"}><PermissionsPane /></Match>
      <Match when={id() === "webhooks"}><WebhooksPane /></Match>
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

  // Raw URL pane — undefined when at /settings (the list root). The mobile
  // branch reads this so no-pane shows the category list instead of forcing
  // Machines.
  const paneSpec = createMemo((): PaneSpec | undefined => {
    const p = params.pane;
    return p ? ALL_PANES.find((x) => x.id === p) : undefined;
  });
  // Desktop rail/content defaults to the first pane (Machines) when none is
  // selected — preserves the existing desktop behavior.
  const activePane = createMemo((): PaneSpec => paneSpec() ?? ALL_PANES[0]!);

  return (
    <Show when={isCompact()} fallback={
      <div class="settings-shell">
        <nav class="settings-rail" aria-label="Settings sections">
          <button type="button" class="settings-rail__brand" aria-label="Back to app" onClick={() => navigate("/")}>Settings</button>
          <For each={GROUPS}>
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
        <For each={GROUPS}>
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
function MobileSettingsDetail(props: { spec: PaneSpec }) {
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
