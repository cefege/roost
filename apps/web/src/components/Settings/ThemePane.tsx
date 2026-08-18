// Settings → Theme pane. Grouped picker over the THEMES registry
// (lib/themes.ts) plus a "System" entry that follows the OS. Each row shows a
// live swatch strip read straight from the theme's canonical tokens, so the
// list previews itself. Click → setTheme (engine applies + persists) + toast.
// Selected state is reactive via currentThemeChoice().
// Callers: SettingsRoot.tsx. Depends on: lib/theme.ts, lib/themes.ts.

import type { Component } from "solid-js";
import { For, createMemo } from "solid-js";
import type { Theme, ThemeGroup } from "../../lib/themeTokens.ts";
import { THEMES, THEMES_BY_ID } from "../../lib/themes.ts";
import { setTheme, currentThemeChoice, resolveThemeId } from "../../lib/theme.ts";
import { addToast } from "../../store/toastStore.ts";
import { Icon } from "./md/primitives.tsx";

interface Entry {
  choice: string;   // "auto" | theme id
  label: string;
  support: string;
  theme: () => Theme; // resolved theme for swatch preview
}

const GROUP_ORDER: ThemeGroup[] = ["System", "Light", "Dark", "Palette"];

// Five canonical tokens that read as a recognisable preview of a theme.
const SWATCH_TOKENS = ["bg-base", "surface-2", "accent", "text-hi", "status-ok"] as const;

export const ThemePane: Component = () => {
  // Build the grouped entry list: a synthetic "System" entry + every theme.
  const groups = createMemo(() => {
    const out: { group: ThemeGroup; entries: Entry[] }[] = [];
    for (const group of GROUP_ORDER) {
      const entries: Entry[] = [];
      if (group === "System") {
        entries.push({
          choice: "auto",
          label: "System",
          support: "Follow the operating-system preference",
          theme: () => THEMES_BY_ID[resolveThemeId("auto")]!,
        });
      }
      for (const t of THEMES.filter((x) => x.group === group)) {
        entries.push({
          choice: t.id, label: t.label,
          support: t.appearance === "dark" ? "Dark" : "Light",
          theme: () => t,
        });
      }
      if (entries.length) out.push({ group, entries });
    }
    return out;
  });

  function pick(choice: string): void {
    if (choice === currentThemeChoice()) return;
    setTheme(choice);
    addToast("Theme saved");
  }

  return (
    <div data-testid="theme-pane" style={{ "max-width": "560px" }}>
      <p class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)", margin: "0 0 18px 2px" }}>
        Pick an appearance. The swatches preview each theme's colors. Applies to every device.
      </p>
      <For each={groups()}>
        {(section) => (
          <div style={{ "margin-bottom": "22px" }}>
            <h3 style={{
              "font-size": "12px", "font-weight": 600, "letter-spacing": "0.04em",
              "text-transform": "uppercase", color: "var(--md-sys-color-on-surface-variant)", margin: "0 0 8px 2px",
            }}>
              {section.group}
            </h3>
            <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-2)" }}>
              <For each={section.entries}>
                {(entry) => {
                  const selected = createMemo(() => currentThemeChoice() === entry.choice);
                  return (
                    <button
                      type="button"
                      data-testid={`theme-row-${entry.choice}`}
                      data-selected={selected() ? "true" : "false"}
                      onClick={() => pick(entry.choice)}
                      style={{
                        display: "flex", "align-items": "center", gap: "var(--md-space-3)",
                        width: "100%", padding: "var(--md-space-3) var(--md-space-3)", "text-align": "left",
                        background: selected() ? "var(--md-sys-color-secondary-container)" : "var(--md-sys-color-surface-container-low)",
                        border: "1px solid var(--md-sys-color-outline-variant)",
                        "border-radius": "var(--md-shape-md)", cursor: "pointer",
                        color: selected() ? "var(--md-sys-color-on-secondary-container)" : "var(--md-sys-color-on-surface)",
                      }}
                    >
                      {/* swatch strip */}
                      <span style={{ display: "flex", "flex-shrink": 0, "border-radius": "var(--md-shape-sm)", overflow: "hidden", border: "1px solid var(--border-subtle)" }}>
                        <For each={SWATCH_TOKENS}>
                          {(tok) => (
                            <span style={{ width: "16px", height: "32px", background: entry.theme().tokens[tok] }} />
                          )}
                        </For>
                      </span>
                      <span style={{ flex: 1, "min-width": 0 }}>
                        <span style={{ display: "block", "font-size": "14px", "font-weight": 600 }}>{entry.label}</span>
                        <span style={{ display: "block", "font-size": "12px", color: "var(--md-sys-color-on-surface-variant)" }}>{entry.support}</span>
                      </span>
                      <Icon name={selected() ? "check_circle" : "radio_button_unchecked"} filled={selected()} />
                    </button>
                  );
                }}
              </For>
            </div>
          </div>
        )}
      </For>
    </div>
  );
};
