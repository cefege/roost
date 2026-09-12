// Help overlay. Keybindings doc + reference.
// Route: /help. Navigates back on Escape.

import { useNavigate } from "@solidjs/router";
import { onMount, onCleanup } from "solid-js";
import { platformShortcutLabel } from "../lib/browserPlatform.ts";

const KEYBINDINGS: Array<{ key: string; action: string }> = [
  { key: platformShortcutLabel("commandPalette", "⌘K"), action: "Command palette" },
  { key: platformShortcutLabel("sidebarSearch", "⌘F"), action: "Search" },
  { key: platformShortcutLabel("toggleSidebar", "⌘B"), action: "Toggle sidebar" },
  { key: "↑ ↓ ↵", action: "Move / open in sidebar" },
  { key: platformShortcutLabel("settings", "⌘,"), action: "Settings" },
  { key: "Shift+?", action: "Help" },
  { key: "Esc", action: "Close overlay" },
];

export function Help() {
  const navigate = useNavigate();

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") navigate(-1);
  }

  onMount(() => window.addEventListener("keydown", onKey));
  onCleanup(() => window.removeEventListener("keydown", onKey));

  return (
    <div class="workbench-help">
      <div class="workbench-help__content">
        <h2 class="workbench-help__title">Help / Keybindings</h2>
        <table class="workbench-help__table">
          <tbody>
            {KEYBINDINGS.map((kb) => (
              <tr>
                <td class="workbench-help__key">{kb.key}</td>
                <td>{kb.action}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <p class="workbench-help__hint">Press Esc to close.</p>
      </div>
    </div>
  );
}
