// NotificationsPane — Settings → Notifications. Toggles for the attention
// notification surfaces (toasts, desktop notifications, tab badge, sound).
// Follows the TranscriptionPane.tsx pattern: Card + SwitchRow, client-only
// prefs from lib/notifyPrefs.ts.
//
// Callers: SettingsRoot.tsx. Depends on: lib/notifyPrefs (reactive prefs +
// setNotifyPref) and lib/push-client (Web Push subscribe/unsubscribe). The
// "Desktop notifications" toggle drives the push SUBSCRIPTION: on = subscribed,
// off = unsubscribed. The desktop pref just records that choice.

import { createSignal, Show } from "solid-js";
import { Card, Switch, Icon } from "./md/primitives.tsx";
import { notifyPrefs, setNotifyPref } from "../../lib/notifyPrefs.ts";
import { subscribeToPush, unsubscribeFromPush, pushAvailable } from "../../lib/push-client.ts";

function SwitchRow(props: { headline: string; support?: string; checked: boolean; onChange: (v: boolean) => void; testId?: string; disabled?: boolean }) {
  return (
    <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-4)" }}>
      <div style={{ flex: 1, "min-width": 0 }}>
        <div class="md-body-m" style={{ color: "var(--md-sys-color-on-surface)" }}>{props.headline}</div>
        <Show when={props.support}>
          <div class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>{props.support}</div>
        </Show>
      </div>
      <Switch
        checked={props.checked}
        onChange={props.onChange}
        label={props.headline}
        testId={props.testId}
      />
    </div>
  );
}

export function NotificationsPane() {
  const [permWarning, setPermWarning] = createSignal("");

  async function toggleDesktop(on: boolean) {
    if (on) {
      if (!pushAvailable()) {
        setPermWarning("Push notifications aren't available in this browser.");
        return;
      }
      try {
        await subscribeToPush();
      } catch (err) {
        setPermWarning(err instanceof Error ? err.message : "Couldn't enable push notifications.");
        return;
      }
    } else {
      await unsubscribeFromPush();
    }
    setPermWarning("");
    setNotifyPref("desktop", on);
  }

  return (
    <div data-testid="settings-notifications-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      {/* ── Status hero ── */}
      <section
        class="md-card"
        data-testid="notifications-status"
        style={{ display: "flex", "align-items": "center", gap: "var(--md-space-4)" }}
      >
        <div
          style={{
            width: "56px", height: "56px", "border-radius": "16px", flex: "0 0 auto",
            display: "grid", "place-items": "center",
            background: "color-mix(in srgb, var(--md-sys-color-primary) 16%, transparent)",
            color: "var(--md-sys-color-primary)",
          }}
        >
          <Icon name="notifications" filled size="lg" />
        </div>
        <div style={{ flex: 1, "min-width": 0 }}>
          <span class="md-title-m" style={{ color: "var(--md-sys-color-on-surface)" }}>
            Attention notifications
          </span>
          <div class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)", "margin-top": "2px" }}>
            Get notified when a terminal finishes working or needs your input. Per-device; applies immediately.
          </div>
        </div>
      </section>

      <Card title="Surfaces">
        <SwitchRow
          headline="In-app toasts"
          support="Slide a toast in the bottom-right corner when a terminal finishes or needs input. Includes a “Jump to it” button."
          checked={notifyPrefs().toast}
          onChange={(v) => setNotifyPref("toast", v)}
          testId="notify-toast-toggle"
        />
        <SwitchRow
          headline="Desktop notifications"
          support="Get an OS notification (even with Roost closed) when a terminal finishes or needs input. Click it to open the session."
          checked={notifyPrefs().desktop}
          onChange={toggleDesktop}
          testId="notify-desktop-toggle"
          disabled={!pushAvailable()}
        />
        <Show when={!pushAvailable()}>
          <div class="md-body-s" style={{ color: "var(--color-warn)", "margin-top": "4px" }}>
            Push notifications aren't available in this browser. On Safari/iOS, add Roost to your Home Screen first.
          </div>
        </Show>
        <Show when={permWarning()}>
          <div class="md-body-s" style={{ color: "var(--color-warn)", "margin-top": "4px" }}>
            {permWarning()}
          </div>
        </Show>
        <SwitchRow
          headline="Tab badge"
          support="Show an unread count in the browser tab title, e.g. “(2) Roost”."
          checked={notifyPrefs().titleBadge}
          onChange={(v) => setNotifyPref("titleBadge", v)}
          testId="notify-title-badge-toggle"
        />
      </Card>

      <Card title="Sound">
        <SwitchRow
          headline="Sound on “needs input”"
          support="Play a short ascending tone when a terminal is waiting for your input."
          checked={notifyPrefs().sound}
          onChange={(v) => setNotifyPref("sound", v)}
          testId="notify-sound-toggle"
        />
        <SwitchRow
          headline="Sound on “finished”"
          support="Also play a tone when a terminal finishes working."
          checked={notifyPrefs().soundOnDone}
          onChange={(v) => setNotifyPref("soundOnDone", v)}
          testId="notify-sound-done-toggle"
        />
      </Card>
    </div>
  );
}
