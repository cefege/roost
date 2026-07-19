// Theme engine. Single source of truth for the active theme.
// Applies a theme by writing the canonical token set (themeTokens.ts) onto
// documentElement.style — inline custom props that win over the :root
// fallback in styles/theme-vars.css, where every other token aliases to them.
//
// Exported: loadTheme (read persisted choice), applyTheme (persist + apply),
// setTheme (alias), currentThemeChoice (reactive accessor for the picker),
// resolveThemeId. Callers: main.tsx (apply before first paint), ThemePane.
// Deps: localStorage, window.matchMedia, document.documentElement, solid-js.

import { createSignal } from "solid-js";
import { CANONICAL_TOKENS } from "./themeTokens.ts";
import { THEMES_BY_ID, SYSTEM_DARK_ID, SYSTEM_LIGHT_ID, DEFAULT_THEME_ID } from "./themes.ts";
import { withViewTransition } from "./viewTransition.ts";

const THEME_STORAGE_KEY = "roost.theme";

// The user's CHOICE — a theme id OR the meta value "auto" (follow OS).
export type ThemeChoice = string;

const [choiceSignal, setChoiceSignal] = createSignal<ThemeChoice>("auto");
/** Reactive accessor — the picker highlights the selected choice. */
export const currentThemeChoice = choiceSignal;

/** Read persisted choice; defaults to "auto". */
export function loadTheme(): ThemeChoice {
  try {
    const stored = localStorage.getItem(THEME_STORAGE_KEY);
    if (stored) return stored;
  } catch {
    // localStorage disabled
  }
  return "auto";
}

function systemThemeId(): string {
  try {
    return window.matchMedia("(prefers-color-scheme: dark)").matches
      ? SYSTEM_DARK_ID : SYSTEM_LIGHT_ID;
  } catch {
    return DEFAULT_THEME_ID;
  }
}

/** Resolve a choice ("auto" or an id) to a concrete theme id. */
export function resolveThemeId(choice: ThemeChoice): string {
  if (choice === "auto") return systemThemeId();
  return THEMES_BY_ID[choice] ? choice : DEFAULT_THEME_ID;
}

/** Persist the choice and apply the resolved theme's tokens immediately. */
export function applyTheme(choice: ThemeChoice): void {
  try {
    localStorage.setItem(THEME_STORAGE_KEY, choice);
  } catch {
    // localStorage disabled
  }
  const id = resolveThemeId(choice);
  const theme = THEMES_BY_ID[id] ?? THEMES_BY_ID[DEFAULT_THEME_ID]!;
  const root = document.documentElement;
  for (const token of CANONICAL_TOKENS) {
    root.style.setProperty(`--${token}`, theme.tokens[token]);
  }
  root.setAttribute("data-theme", id);
  root.style.colorScheme = theme.appearance;
  setChoiceSignal(choice);
}

/** Picker entry point — cross-fades the whole-app recolor via the View
 *  Transitions API (Material "You" feel). applyTheme stays instant for boot +
 *  the OS auto-flip; only an explicit user pick animates. */
export function setTheme(choice: ThemeChoice): void {
  withViewTransition(() => applyTheme(choice));
}

// Re-apply when the OS scheme flips while the user is on "auto".
try {
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    if (loadTheme() === "auto") applyTheme("auto");
  });
} catch {
  // matchMedia unavailable (SSR / old engine)
}
