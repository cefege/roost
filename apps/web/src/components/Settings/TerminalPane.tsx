// TerminalPane — terminal behavior settings (Settings → Terminal). Client-only
// prefs, persisted per device, reactive (apply immediately, no reload):
//  - keyboard resize: shrink the terminal to fit above the soft keyboard
//    (default OFF — push is the default) vs push it up off-screen (lib/keyboardResizePref).
//  - mouse mode: forward clicks/scroll/swipe to a fullscreen app
//    vs native browser select/scroll (lib/mouseForwardPref).
// Callers: SettingsRoot.tsx.

import { Show } from "solid-js";
import { Card, Switch, IconButton, Button } from "./md/primitives.tsx";
import { Select } from "./md/Select.tsx";
import { keyboardResize, setKeyboardResize } from "../../lib/keyboardResizePref.ts";
import { mouseForwardEnabled, toggleMouseForward } from "../../lib/mouseForwardPref.ts";
import { predictMode, setPredictMode } from "../../lib/predictPref.ts";
import {
  termFontSize, stepTermFontSize, resetTermFontSize,
  TERM_FONT_MIN_PX, TERM_FONT_MAX_PX,
} from "../../lib/terminalFontPref.ts";
import { copyOnSelect, setCopyOnSelect } from "../../lib/copyOnSelectPref.ts";

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


      <Card title="Mouse mode">
        <SwitchRow
          headline="Forward mouse + touch to fullscreen apps"
          support="On: clicks, scroll, and finger-swipes go to the fullscreen app instead of the browser. Off: native browser selection and scrolling. This device only."
          checked={mouseForwardEnabled()}
          onChange={toggleMouseForward}
          testId="mouse-forward-toggle"
        />
      </Card>
      <Card title="Selection">
        <SwitchRow
          headline="Copy on select"
          support="On: releasing a selection in the terminal puts it on the clipboard immediately (the tmux/xterm habit). Off: copy explicitly with ⌘/Ctrl+Shift+C or the right-click menu. Off by default because it overwrites the system clipboard without being asked. This device only."
          checked={copyOnSelect()}
          onChange={setCopyOnSelect}
          testId="copy-on-select-toggle"
        />
      </Card>
      <Card title="Text size">
        <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-3)" }}>
          <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-3)" }}>
            <IconButton
              icon="remove"
              label="Smaller terminal text"
              data-testid="term-font-smaller"
              disabled={termFontSize() <= TERM_FONT_MIN_PX}
              onClick={() => stepTermFontSize(-1)}
            />
            <span class="md-body-m" data-testid="term-font-size" style={{ color: "var(--md-sys-color-on-surface)", "min-width": "var(--md-space-8)", "text-align": "center" }}>
              {termFontSize()}px
            </span>
            <IconButton
              icon="add"
              label="Larger terminal text"
              data-testid="term-font-larger"
              disabled={termFontSize() >= TERM_FONT_MAX_PX}
              onClick={() => stepTermFontSize(1)}
            />
            <Button
              variant="text"
              data-testid="term-font-reset"
              onClick={resetTermFontSize}
            >
              Reset
            </Button>
          </div>
          <p class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)", margin: "0" }}>
            Also ⌘ / Ctrl with + , − or 0 while a terminal is on screen. Changing the
            text size changes how many columns and rows fit, so every open terminal
            re-sizes its shell to match. This device only.
          </p>
        </div>
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
            Paints each typed character immediately and reconciles it when the terminal confirms. Adaptive only engages on a high-latency link; Always shows it everywhere except fullscreen apps (for example vim); Experimental shows guesses instantly but may flicker. This device only; applies immediately.
          </p>
        </div>
      </Card>
    </div>
  );
}
