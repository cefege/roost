// HelpOverlay — Shift+? modal. Lists all keyboard shortcuts grouped by category.
// Text filter + "Copy diagnostic" button (copies URL/UA/time to clipboard).
// Open/close state from lib/keyboardShortcuts.ts.
//
// Callers: App.tsx (always rendered; internally gated on helpOpen).
// Depends on: keyboardShortcuts signals only — no store reads.

import { createMemo, createSignal, For, Show } from "solid-js";
import type { JSX } from "solid-js";
import { helpOpen, closeHelp } from "../lib/keyboardShortcuts.ts";
import { Button, TextField } from "./Settings/md/primitives.tsx";
import { Sheet } from "./Settings/md/Sheet.tsx";
import { copyToClipboard } from "../lib/clipboard.ts";
import { platformShortcutLabel } from "../lib/browserPlatform.ts";
import { credentialFreeUrl } from "../auth/fragment-credential.ts";

// ── Static shortcut catalogue ─────────────────────────────────────────────────

interface ShortcutEntry {
  category: string;
  label: string;
  binding: string;
  description?: string;
}

const SHORTCUTS: ShortcutEntry[] = [
  // Navigation
  { category: "Navigation", label: "Command palette", binding: platformShortcutLabel("commandPalette", "⌘K") },
  { category: "Navigation", label: "Filter the sidebar", binding: platformShortcutLabel("sidebarSearch", "⌘F (no terminal on screen) / Ctrl+F") },
  { category: "Navigation", label: "Toggle sidebar", binding: platformShortcutLabel("toggleSidebar", "⌘B") },
  { category: "Navigation", label: "Move / open in sidebar", binding: "↑ ↓ ↵" },
  { category: "Navigation", label: "Move focus to the adjacent pane", binding: platformShortcutLabel("paneFocus", "⌘⌥← ↑ → ↓ / Ctrl+Alt+← ↑ → ↓") },
  { category: "Navigation", label: "Help", binding: "Shift+?" },
  { category: "Navigation", label: "Close modal / Escape", binding: "Esc" },
  // Terminal
  { category: "Terminal", label: "Context menu", binding: "Right-click" },
  { category: "Terminal", label: "New terminal in the focused pane (same folder & server)", binding: platformShortcutLabel("newTerminal", "⌘⌥T / Ctrl+Alt+T") },
  { category: "Terminal", label: "Focus tab 1–8 / last tab in the focused pane", binding: platformShortcutLabel("terminalTab", "⌘1–⌘8 / ⌘9 · Ctrl+1–8 / Ctrl+9") },
  { category: "Terminal", label: "Kill session", binding: "context menu" },
  { category: "Terminal", label: "Bring pane to front / push back", binding: platformShortcutLabel("spotlight", "⌘↵ / middle-click / right-click") },
  { category: "Terminal", label: "Split right / split down", binding: `${platformShortcutLabel("splitRight", "⌘D")} / ${platformShortcutLabel("splitDown", "⌘⇧D")}` },
  { category: "Terminal", label: "Arrange — equalize pane sizes", binding: platformShortcutLabel("arrangeBalance", "Cmd+Opt+B") },
  { category: "Terminal", label: "Arrange — grid / columns / rows / main+stack", binding: `${platformShortcutLabel("arrangeGrid", "Cmd+Opt+G")} / E / R / V` },
  { category: "Terminal", label: "Copy selection / paste", binding: `${platformShortcutLabel("terminalCopy", "⌘⇧C")} / ${platformShortcutLabel("terminalPaste", "⌘⇧V")}` },
  { category: "Terminal", label: "Text size — bigger / smaller / reset", binding: `${platformShortcutLabel("termFontIncrease", "⌘+")} / ${platformShortcutLabel("termFontDecrease", "⌘−")} / ${platformShortcutLabel("termFontReset", "⌘0")}` },
  { category: "Terminal", label: "Find in scrollback", binding: platformShortcutLabel("terminalFind", "⌘F / Ctrl+⇧F") },
  { category: "Terminal", label: "Find next / previous match", binding: "↵ / ⇧↵ · ⌘G / ⌘⇧G" },
  { category: "Terminal", label: "Close find", binding: "Esc" },
  // Settings
  { category: "Settings", label: "Open Settings", binding: platformShortcutLabel("settings", "⌘,") },
];

// ── Component ─────────────────────────────────────────────────────────────────

export function HelpOverlay() {
  const [filter, setFilter] = createSignal("");
  let inputRef: HTMLElement | undefined;

  createMemo(() => {
    if (helpOpen()) requestAnimationFrame(() => inputRef?.focus());
  });

  const filteredShortcuts = createMemo<ShortcutEntry[]>(() => {
    const q = filter().toLowerCase().trim();
    if (!q) return SHORTCUTS;
    return SHORTCUTS.filter(
      (shortcut) =>
        shortcut.label.toLowerCase().includes(q) ||
        shortcut.category.toLowerCase().includes(q) ||
        (shortcut.description?.toLowerCase().includes(q) ?? false),
    );
  });

  const grouped = createMemo(() => {
    const groups = new Map<string, ShortcutEntry[]>();
    for (const shortcut of filteredShortcuts()) {
      const shortcuts = groups.get(shortcut.category) ?? [];
      shortcuts.push(shortcut);
      groups.set(shortcut.category, shortcuts);
    }
    return groups;
  });

  return (
    <Sheet
      open={helpOpen()}
      onClose={closeHelp}
      headline="Keyboard shortcuts"
      side="center"
      class="roost-dialog--wide roost-dialog--help"
    >
      <Show when={helpOpen()}>
        <div data-testid="help-overlay" class="roost-help-overlay">
          <div class="roost-help-overlay__tools">
            <Button variant="secondary" data-testid="help-overlay-copy-diagnostic"
              onClick={() => void copyDiagnostic()}>
              Copy diagnostic
            </Button>
          </div>

          <div style={{ padding: "var(--md-space-3) var(--md-space-5) var(--md-space-2)" }}>
            <TextField
              ref={(element) => { inputRef = element; }}
              value={filter()}
              onInput={setFilter}
              placeholder="Filter shortcuts…"
              ariaLabel="Filter shortcuts"
              testId="help-overlay-filter"
              style={{ width: "100%" }}
            />
          </div>

          <div style={{ flex: "1", "min-height": "0", "overflow-y": "auto", padding: "var(--md-space-2) var(--md-space-5) var(--md-space-4)" }}>
            <Show when={filteredShortcuts().length === 0}>
              <p class="md-body-s" style={{ color: "var(--text-lo)", "margin-top": "var(--md-space-2)" }}>
                No matches.
              </p>
            </Show>
            <For each={Array.from(grouped().entries())}>
              {([category, shortcuts]) => (
                <section style={{ "margin-bottom": "var(--md-space-4)" }}>
                  <h2
                    class="md-label-s"
                    style={{
                      "text-transform": "uppercase",
                      color: "var(--text-lo)",
                      "margin-bottom": "var(--md-space-1)",
                    }}
                  >
                    {category}
                  </h2>
                  <ul style={{ "list-style": "none", margin: "0", padding: "0" }}>
                    <For each={shortcuts}>
                      {(shortcut) => (
                        <li
                          class="md-body-s"
                          data-testid="help-overlay-action"
                          data-action-id={`${shortcut.category}:${shortcut.label}`}
                          style={{
                            display: "flex",
                            "justify-content": "space-between",
                            "align-items": "center",
                            gap: "var(--md-space-2)",
                            padding: "var(--md-space-1) 0",
                          }}
                        >
                          <span style={{ color: "var(--text-hi)" }}>{shortcut.label}</span>
                          <Show when={shortcut.binding}>
                            <BindingChip>{shortcut.binding}</BindingChip>
                          </Show>
                        </li>
                      )}
                    </For>
                  </ul>
                </section>
              )}
            </For>
          </div>
        </div>
      </Show>
    </Sheet>
  );
}

// ── Sub-components ─────────────────────────────────────────────────────────────

function BindingChip(props: { children: JSX.Element }) {
  return (
    <kbd style={{
      font: "var(--md-label-s-weight) var(--md-label-s-size)/var(--md-label-s-line) var(--font-mono)",
      padding: "var(--md-space-1) var(--md-space-2)",
      "border-radius": "var(--md-shape-xs)",
      background: "var(--surface-1)",
      border: "var(--workbench-border-width) solid var(--md-sys-color-outline-variant)",
      color: "var(--text-mid)",
      "white-space": "nowrap",
    }}>
      {props.children}
    </kbd>
  );
}

// ── Diagnostic copy ────────────────────────────────────────────────────────────

async function copyDiagnostic(): Promise<void> {
  const payload = {
    url: typeof window !== "undefined" ? credentialFreeUrl(window.location) : "",
    ua: typeof navigator !== "undefined" ? navigator.userAgent : "",
    time: new Date().toISOString(),
  };
  // Clipboard write may fail in insecure context; non-fatal — the payload
  // fields are all visible in the overlay itself.
  await copyToClipboard(JSON.stringify(payload, null, 2));
}
