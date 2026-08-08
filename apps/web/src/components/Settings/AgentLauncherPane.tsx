// Settings → Agents → Launcher pane. Picks the agent Roost auto-launches in
// new terminals (lib/spawnSession.ts::maybeAutoLaunchAgent). Built-in selection saves immediately
// (ThemePane feel); the Custom command needs an explicit Save (free text).
// Server-stored via lib/agents.ts (app_settings KV), so the choice applies to
// every device. Callers: SettingsRoot.tsx.

import { type Component, createSignal, Show } from "solid-js";
import { Select, TextField, Button, Switch } from "./md/primitives.tsx";
import { addToast } from "../../lib/toastStore.ts";
import { AgentTile } from "../AgentGlyph.tsx";
import {
  BUILTIN_AGENTS, resolveAgentFrom, currentSelected, currentCustomCommand, saveAgentConfig,
  autoLaunchEnabled, saveAutoLaunch,
} from "../../lib/agents.ts";

export const AgentLauncherPane: Component = () => {
  const [draftSelected, setDraftSelected] = createSignal(currentSelected());
  const [draftCustom, setDraftCustom] = createSignal(currentCustomCommand());

  const options = [
    ...BUILTIN_AGENTS.map((a) => ({ value: a.id, label: a.label })),
    { value: "custom", label: "Custom command…" },
  ];

  const resolved = () => resolveAgentFrom(draftSelected(), draftCustom());

  async function onSelect(v: string): Promise<void> {
    setDraftSelected(v);
    if (v !== "custom") {
      await saveAgentConfig(v, currentCustomCommand());
      addToast("Default agent saved");
    }
  }

  async function saveCustom(): Promise<void> {
    await saveAgentConfig("custom", draftCustom().trim());
    addToast("Default agent saved");
  }

  return (
    <div data-testid="agent-launcher-pane" style={{ "max-width": "560px" }}>
      <p class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)", margin: "0 0 18px 2px" }}>
        Pick the agent Roost auto-launches in new terminals. Applies to every device.
      </p>

      <Select
        value={draftSelected()}
        onChange={(v) => void onSelect(v)}
        label="Default agent"
        options={options}
        testId="agent-select"
      />

      <Show when={draftSelected() === "custom"}>
        <div style={{ display: "flex", "align-items": "flex-end", gap: "12px", "margin-top": "16px" }}>
          <TextField
            value={draftCustom()}
            onInput={setDraftCustom}
            label="Custom command"
            placeholder="e.g. aider --model sonnet"
            testId="agent-custom-command"
            style={{ flex: "1 1 auto" }}
          />
          <Button
            variant="filled"
            data-testid="agent-custom-save"
            disabled={draftCustom().trim() === ""}
            onClick={() => void saveCustom()}
          >
            Save
          </Button>
        </div>
      </Show>

      <div style={{ display: "flex", "align-items": "center", gap: "12px", "margin-top": "22px" }}>
        <AgentTile agent={resolved()} />
        <span class="md-body-m" style={{ "font-weight": "600" }}>{resolved().label}</span>
        <code style={{
          "font-family": "var(--font-mono)", "font-size": "13px",
          padding: "3px 8px", "border-radius": "6px",
          background: "var(--md-sys-color-surface-container-high)",
          color: "var(--md-sys-color-on-surface)",
        }}>
          {`${resolved().command}⏎`}
        </code>
      </div>

      <div style={{ "margin-top": "22px", display: "flex", "align-items": "center", gap: "12px" }}>
        <Switch
          checked={autoLaunchEnabled()}
          onChange={(v) => void saveAutoLaunch(v)}
          label="Auto-launch agent in new terminal windows"
          testId="agent-auto-launch-toggle"
        />
        <span class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>
          Auto-launch agent in new terminal windows
        </span>
      </div>
    </div>
  );
};
