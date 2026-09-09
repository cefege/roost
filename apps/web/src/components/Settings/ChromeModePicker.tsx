// Settings → Theme pane interface selector. Owns the browser-profile choice
// between Roost and Workbench visual grammar without touching terminal state.
// ThemePane composes it; chromeMode.ts owns persistence and document state.
// Native radios preserve keyboard and assistive-technology selection semantics.

import { type Component, For, Show } from "solid-js";
import type { ChromeMode } from "../../lib/chromeMode.ts";
import { currentChromeMode, setChromeMode } from "../../lib/chromeMode.ts";
import { Card, Icon } from "./md/primitives.tsx";
import "./ChromeModePicker.css";

interface ChromeModeOption {
  mode: ChromeMode;
  title: string;
  support: string;
}

const CHROME_MODE_OPTIONS: readonly ChromeModeOption[] = [
  {
    mode: "roost",
    title: "Roost",
    support: "The familiar Roost interface.",
  },
  {
    mode: "workbench",
    title: "Workbench",
    support: "Dense editor-workbench chrome for navigation and panels.",
  },
];

export const ChromeModePicker: Component = () => (
  <Card class="chrome-mode-picker">
    <fieldset class="chrome-mode-picker__fieldset" aria-describedby="chrome-mode-picker-support">
      <legend class="chrome-mode-picker__title">Interface</legend>
      <p id="chrome-mode-picker-support" class="chrome-mode-picker__support">
        Applies only to this browser profile. Terminal sessions and palette colors stay unchanged.
      </p>
      <div class="chrome-mode-picker__options">
        <For each={CHROME_MODE_OPTIONS}>
          {(option) => {
            const descriptionId = `chrome-mode-${option.mode}-support`;
            return (
              <label class="chrome-mode-picker__option" data-selected={currentChromeMode() === option.mode ? "true" : "false"}>
                <input
                  class="chrome-mode-picker__option-control"
                  type="radio"
                  name="chrome-mode"
                  value={option.mode}
                  checked={currentChromeMode() === option.mode}
                  aria-describedby={descriptionId}
                  onChange={() => setChromeMode(option.mode)}
                />
                <span>
                  <span class="chrome-mode-picker__option-title">{option.title}</span>
                  <span id={descriptionId} class="chrome-mode-picker__option-support">{option.support}</span>
                </span>
                <Show when={currentChromeMode() === option.mode}>
                  <Icon class="chrome-mode-picker__option-mark" name="check_circle" filled />
                </Show>
              </label>
            );
          }}
        </For>
      </div>
    </fieldset>
  </Card>
);
