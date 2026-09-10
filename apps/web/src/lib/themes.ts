// Theme registry for the two supported appearances.
// Existing app surface, status, syntax, and ANSI roles remain stable while
// wb-* roles mirror the VS Code 1.137.0 Dark Modern and Light Modern chrome.
// lib/theme.ts applies every canonical token to documentElement; styles/
// theme-vars.css aliases the rest of the app to those tokens.

import type { Theme } from "./themeTokens.ts";

// Dark appearance: the existing app palette plus VS Code Dark Modern chrome.
const DARK: Theme = {
  id: "graphite", label: "Dark", group: "Dark", appearance: "dark",
  tokens: {
    "bg-base": "#141a1e", "surface-0": "#0a0f11", "surface-1": "#141a1e",
    "surface-2": "#1f272c", "surface-3": "#242d33", "term-bg": "#0a0f11",
    "text-hi": "#e3e3e1", "text-mid": "#c1c9cc", "text-lo": "#8c969b", "term-fg": "#e3e3e1",
    "accent": "#7fd1ec", "on-accent": "#00363f", "accent-container": "#20586e", "on-accent-container": "#bde9ff",
    "border-strong": "#5a646a", "border-subtle": "#2a343a",
    "status-ok": "#81c995", "status-warn": "#fdd663", "status-err": "#f28b82", "status-info": "#7fd1ec",
    "syntax-plain": "#e3e3e1", "syntax-keyword": "#7fd1ec", "syntax-string": "#81c995", "syntax-number": "#fdd663", "syntax-comment": "#8c969b",
    "secondary-container": "#242d33", "on-secondary-container": "#e3e3e1",
    // VS Code 1.137.0 Dark Modern workbench roles.
    "wb-titlebar": "#181818", "wb-activity": "#181818", "wb-sidebar": "#181818",
    "wb-editor": "#1f1f1f", "wb-statusbar": "#181818", "wb-border": "#2b2b2b",
    "wb-active": "#0078d4", "wb-active-contrast": "#ffffff", "wb-focus": "#0078d4",
    "wb-selected": "#37373d", "wb-selected-contrast": "#ffffff",
    "wb-tab-active": "#1f1f1f", "wb-tab-inactive": "#181818", "wb-tab-hover": "#1f1f1f",
    "wb-pane-drop": "#53595d80", "wb-pane-focus": "#0078d4",
    "ansi-black": "#242d33", "ansi-red": "#f28b82", "ansi-green": "#81c995", "ansi-yellow": "#fdd663",
    "ansi-blue": "#7fd1ec", "ansi-magenta": "#c58af9", "ansi-cyan": "#78d9ec", "ansi-white": "#e3e3e1",
    "ansi-bright-black": "#5a646a", "ansi-bright-red": "#f6aea9", "ansi-bright-green": "#a8dab5", "ansi-bright-yellow": "#fde293",
    "ansi-bright-blue": "#bde9ff", "ansi-bright-magenta": "#d7aefb", "ansi-bright-cyan": "#a1e4f2", "ansi-bright-white": "#ffffff",
  },
};

// Light appearance: the existing app palette plus VS Code Light Modern chrome.
const LIGHT: Theme = {
  id: "light", label: "Light", group: "Light", appearance: "light",
  tokens: {
    "bg-base": "#e9eef4", "surface-0": "#ffffff", "surface-1": "#e9eef4",
    "surface-2": "#dde3ea", "surface-3": "#cdd5de", "term-bg": "#ffffff",
    "text-hi": "#1f1f1f", "text-mid": "#444746", "text-lo": "#5f6368", "term-fg": "#1f1f1f",
    "accent": "#0b57d0", "on-accent": "#ffffff", "accent-container": "#a1d6f7", "on-accent-container": "#001d35",
    "border-strong": "#747775", "border-subtle": "#c4c7c5",
    "status-ok": "#188038", "status-warn": "#b06000", "status-err": "#c5221f", "status-info": "#0b57d0",
    "syntax-plain": "#383a42", "syntax-keyword": "#a626a4", "syntax-string": "#50a14f", "syntax-number": "#c18401", "syntax-comment": "#a0a1a7",
    "secondary-container": "#c2e7ff", "on-secondary-container": "#001d35",
    // VS Code 1.137.0 Light Modern workbench roles.
    "wb-titlebar": "#f8f8f8", "wb-activity": "#f8f8f8", "wb-sidebar": "#f8f8f8",
    "wb-editor": "#ffffff", "wb-statusbar": "#f8f8f8", "wb-border": "#e5e5e5",
    "wb-active": "#005fb8", "wb-active-contrast": "#ffffff", "wb-focus": "#005fb8",
    "wb-selected": "#e4e6f1", "wb-selected-contrast": "#333333",
    "wb-tab-active": "#ffffff", "wb-tab-inactive": "#f8f8f8", "wb-tab-hover": "#ffffff",
    "wb-pane-drop": "#2677cb2e", "wb-pane-focus": "#005fb8",
    "ansi-black": "#383a42", "ansi-red": "#e45649", "ansi-green": "#50a14f", "ansi-yellow": "#c18401",
    "ansi-blue": "#4078f2", "ansi-magenta": "#a626a4", "ansi-cyan": "#0184bc", "ansi-white": "#a0a1a7",
    "ansi-bright-black": "#696c77", "ansi-bright-red": "#e45649", "ansi-bright-green": "#50a14f", "ansi-bright-yellow": "#986801",
    "ansi-bright-blue": "#4078f2", "ansi-bright-magenta": "#a626a4", "ansi-bright-cyan": "#0184bc", "ansi-bright-white": "#ffffff",
  },
};

export const THEMES: Theme[] = [LIGHT, DARK];

export const THEMES_BY_ID: Record<string, Theme> =
  Object.fromEntries(THEMES.map((t) => [t.id, t]));

/** The theme an `auto`/System selection resolves to for each appearance. */
export const SYSTEM_DARK_ID = "graphite";
export const SYSTEM_LIGHT_ID = "light";

export const DEFAULT_THEME_ID = "graphite";
