// TerminalPane — terminal behavior settings (Settings → Terminal). Client-only
// prefs, persisted per device, reactive (apply immediately, no reload):
//  - keyboard resize: shrink the terminal to fit above the soft keyboard
//    (default OFF — push is the default) vs push it up off-screen (lib/keyboardResizePref).
//  - key pad on desktop: show the on-screen nav pad on desktop widths
//    (default ON) (lib/keyboardOnDesktop).
//  - mouse mode: forward clicks/scroll/swipe to the running app (claude
//    fullscreen) vs native browser select/scroll (lib/mouseForwardPref). Also
//    toggled from the on-screen nav pad; this is the desktop-reachable home.
// Callers: SettingsRoot.tsx.

import { Show } from "solid-js";
import { Card, Switch } from "./md/primitives.tsx";
import { Select } from "./md/Select.tsx";
import { keyboardResize, setKeyboardResize } from "../../lib/keyboardResizePref.ts";
import { keyboardOnDesktop, setKeyboardOnDesktop } from "../../lib/keyboardOnDesktop.ts";
import { mouseForwardEnabled, toggleMouseForward } from "../../lib/mouseForwardPref.ts";
import { predictMode, setPredictMode } from "../../lib/predictPref.ts";

function SwitchRow(props: { headline: string; support?: string; checked: boolean; onChange: (v: boolean) => void; testId?: string }) {
  return (
    <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-4)" }}>
      <div style={{ flex: 1, "min-width": 0 }}>
        <div class="md-body-m" style={{ color: "var(--md-sys-color-on-surface)" }}>{props.headline}</div>
        <Show when={props.support}>
          <div class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>{props.support}</div>
        </Show>
      </div>
      <Switch checked={props.checked} onChange={props.onChange} testId={props.testId} label={props.headline} />
    </div>
  );
}

export function TerminalPane() {
  return (
    <div data-testid="settings-terminal-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <p class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)", margin: "0" }}>
        How the terminal behaves on this device. Every setting applies immediately and is saved per device.
      </p>
      <Card title="Soft keyboard">
        <SwitchRow
          headline="Resize terminal when the keyboard opens"
          support="On: the terminal shrinks to fit above the on-screen keyboard and grows back when it closes. Off: the terminal keeps its size and slides up so the input stays visible (the top scrolls off). This device only."
          checked={keyboardResize()}
          onChange={setKeyboardResize}
          testId="keyboard-resize-toggle"
        />
      </Card>

      <Card title="On-screen key pad">
        <SwitchRow
          headline="Show the key pad on desktop"
          support="The nav pad (esc, arrows, enter, mouse toggle) floats bottom-right of the terminal, behind a keyboard-icon button. It always appears on compact/mobile widths — turn this off to hide it on desktop. This device only; applies immediately."
          checked={keyboardOnDesktop()}
          onChange={setKeyboardOnDesktop}
          testId="keyboard-on-desktop-toggle"
        />
      </Card>

      <Card title="Mouse mode">
        <SwitchRow
          headline="Forward mouse + touch to fullscreen apps"
          support="On: clicks, scroll, and finger-swipes go to the running app (e.g. scroll claude's fullscreen) instead of the browser. Off: native browser selection and scrolling. Also toggleable from the on-screen key pad. This device only."
          checked={mouseForwardEnabled()}
          onChange={toggleMouseForward}
          testId="mouse-forward-toggle"
        />
      </Card>
      <Card title="Local echo">
        <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-3)" }}>
          <Select
            label="Predictive local echo"
            testId="predict-mode-select"
            value={predictMode()}
            onChange={setPredictMode}
            options={[
              { value: "adaptive", label: "Adaptive (slow links only)" },
              { value: "always", label: "Always" },
              { value: "experimental", label: "Experimental (aggressive)" },
              { value: "never", label: "Never" },
            ]}
          />
          <p class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)", margin: "0" }}>
            Paints each typed character immediately and reconciles it when the terminal confirms. Adaptive only engages on a high-latency link; Always shows it everywhere except fullscreen apps (claude/vim); Experimental shows guesses instantly but may flicker. This device only; applies immediately.
          </p>
        </div>
      </Card>
    </div>
  );
}
