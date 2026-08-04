// Settings → Notifications. Browser-local delivery preferences; desktop Web
// Push is enabled only after permission and coordinator subscription succeed.

import { createMemo, createSignal, Show } from "solid-js";
import { Card, Switch, Icon } from "./md/primitives.tsx";
import {
  disableDesktopNotifications,
  enableDesktopNotifications,
  notifyPrefs,
  setNotifyPref,
} from "../../lib/notifyPrefs.ts";
import {
  pushAvailable,
  subscribeToPush,
  unsubscribeFromPush,
} from "../../lib/push-client.ts";

function SwitchRow(props: {
  headline: string;
  support: string;
  checked: boolean;
  onChange: (value: boolean) => void;
  testId: string;
  disabled?: boolean;
}) {
  return (
    <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-4)" }}>
      <div style={{ flex: 1, "min-width": 0 }}>
        <div class="md-body-m" style={{ color: "var(--md-sys-color-on-surface)" }}>
          {props.headline}
        </div>
        <div class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>
          {props.support}
        </div>
      </div>
      <Switch
        checked={props.checked}
        onChange={props.onChange}
        label={props.headline}
        testId={props.testId}
        disabled={props.disabled}
      />
    </div>
  );
}

export function NotificationsPane() {
  const initialPermission = pushAvailable() ? Notification.permission : "unsupported";
  const [permission, setPermission] = createSignal<NotificationPermission | "unsupported">(
    initialPermission,
  );
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");

  const desktopStatus = createMemo(() => {
    if (!pushAvailable()) {
      return "Desktop notifications are unavailable in this browser. On Safari and iOS, install Roost to the Home Screen first.";
    }
    if (permission() === "denied") {
      return "Notifications are blocked. Allow Roost in this browser's site settings, then try again.";
    }
    if (busy()) return "Updating this browser's notification subscription…";
    if (notifyPrefs().desktop && permission() === "granted") {
      return "Enabled for this browser. The coordinator suppresses pushes while this device is viewing the terminal.";
    }
    return "Get an OS notification even when Roost is closed. Enabling asks for browser permission.";
  });

  const toggleDesktop = async (enabled: boolean): Promise<void> => {
    if (busy()) return;
    setError("");
    setBusy(true);
    try {
      if (enabled) {
        await enableDesktopNotifications(subscribeToPush);
        setPermission(Notification.permission);
      } else {
        await disableDesktopNotifications(unsubscribeFromPush);
        if (pushAvailable()) setPermission(Notification.permission);
      }
    } catch (reason) {
      if (pushAvailable()) setPermission(Notification.permission);
      setError(reason instanceof Error ? reason.message : "Desktop notifications could not be updated.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      data-testid="settings-notifications-pane"
      style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}
    >
      <section
        class="md-card"
        data-testid="notifications-status"
        style={{ display: "flex", "align-items": "center", gap: "var(--md-space-4)" }}
      >
        <div style={{
          width: "56px",
          height: "56px",
          "border-radius": "16px",
          flex: "0 0 auto",
          display: "grid",
          "place-items": "center",
          background: "color-mix(in srgb, var(--md-sys-color-primary) 16%, transparent)",
          color: "var(--md-sys-color-primary)",
        }}>
          <Icon name="notifications" filled size="lg" />
        </div>
        <div style={{ flex: 1, "min-width": 0 }}>
          <div class="md-title-m" style={{ color: "var(--md-sys-color-on-surface)" }}>
            Agent notifications
          </div>
          <div class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>
            Know when a background coding agent needs input or finishes. Saved per browser.
          </div>
        </div>
      </section>

      <Card title="Surfaces">
        <SwitchRow
          headline="In-app toasts"
          support="Show a delayed toast with a View action when a background agent needs input or finishes."
          checked={notifyPrefs().inApp}
          onChange={(value) => setNotifyPref("inApp", value)}
          testId="notify-in-app-toggle"
        />
        <SwitchRow
          headline="Desktop notifications"
          support={desktopStatus()}
          checked={notifyPrefs().desktop}
          onChange={(value) => { void toggleDesktop(value); }}
          testId="notify-desktop-toggle"
          disabled={busy() || !pushAvailable() || permission() === "denied"}
        />
        <Show when={error()}>
          <div class="md-body-s" role="alert" style={{ color: "var(--md-warning)" }}>
            {error()}
          </div>
        </Show>
        <SwitchRow
          headline="Tab title badge"
          support="Prefix the Roost tab title with the number of unseen needs-input and finished states."
          checked={notifyPrefs().titleBadge}
          onChange={(value) => setNotifyPref("titleBadge", value)}
          testId="notify-title-badge-toggle"
        />
      </Card>

      <Card title="Sound">
        <SwitchRow
          headline="Sound when input is needed"
          support="Play two short ascending tones after a background agent starts waiting for you."
          checked={notifyPrefs().blockedSound}
          onChange={(value) => setNotifyPref("blockedSound", value)}
          testId="notify-blocked-sound-toggle"
        />
        <SwitchRow
          headline="Sound when finished"
          support="Play a short tone after a background agent completes its work."
          checked={notifyPrefs().doneSound}
          onChange={(value) => setNotifyPref("doneSound", value)}
          testId="notify-done-sound-toggle"
        />
      </Card>
    </div>
  );
}
