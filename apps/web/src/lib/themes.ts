// Theme registry for the two supported appearances.
// Existing app surface, status, syntax, and ANSI roles remain stable while
// wb-* roles mirror the VS Code 1.137.0 Dark Modern and Light Modern chrome.
// lib/theme.ts applies every canonical token to documentElement; styles/
// theme-vars.css aliases the rest of the app to those tokens.

import type { Theme } from "./themeTokens.ts";

// Dark appearance: restrained neutral chrome while terminal/status/syntax roles stay stable.
const DARK: Theme = {
  id: "graphite", label: "Dark", group: "Dark", appearance: "dark",
  tokens: {
    "bg-base": "#0a0a0a", "surface-0": "#0a0a0a", "surface-1": "#171717",
    "surface-2": "#262626", "surface-3": "#404040", "term-bg": "#0a0f11",
    "text-hi": "#fafafa", "text-mid": "#d4d4d4", "text-lo": "#a3a3a3", "term-fg": "#e3e3e1",
    "accent": "#fafafa", "on-accent": "#171717", "accent-container": "#262626", "on-accent-container": "#fafafa",
    "border-strong": "#525252", "border-subtle": "#262626",
    "status-ok": "#81c995", "status-warn": "#fdd663", "status-err": "#f28b82", "status-info": "#7fd1ec",
    "syntax-plain": "#e3e3e1", "syntax-keyword": "#7fd1ec", "syntax-string": "#81c995", "syntax-number": "#fdd663", "syntax-comment": "#8c969b",
    "secondary-container": "#262626", "on-secondary-container": "#fafafa",
    "wb-titlebar": "#171717", "wb-activity": "#171717", "wb-sidebar": "#171717",
    "wb-editor": "#0a0a0a", "wb-statusbar": "#171717", "wb-border": "#262626",
    "wb-active": "#fafafa", "wb-active-contrast": "#0a0a0a", "wb-focus": "#fafafa",
    "wb-selected": "#262626", "wb-selected-contrast": "#fafafa",
    "wb-tab-active": "#0a0a0a", "wb-tab-inactive": "#171717", "wb-tab-hover": "#262626",
    "wb-pane-drop": "#fafafa26", "wb-pane-focus": "#fafafa",
    "ansi-black": "#242d33", "ansi-red": "#f28b82", "ansi-green": "#81c995", "ansi-yellow": "#fdd663",
    "ansi-blue": "#7fd1ec", "ansi-magenta": "#c58af9", "ansi-cyan": "#78d9ec", "ansi-white": "#e3e3e1",
    "ansi-bright-black": "#5a646a", "ansi-bright-red": "#f6aea9", "ansi-bright-green": "#a8dab5", "ansi-bright-yellow": "#fde293",
    "ansi-bright-blue": "#bde9ff", "ansi-bright-magenta": "#d7aefb", "ansi-bright-cyan": "#a1e4f2", "ansi-bright-white": "#ffffff",
  },
};

// Light appearance: restrained neutral chrome while terminal/status/syntax roles stay stable.
const LIGHT: Theme = {
  id: "light", label: "Light", group: "Light", appearance: "light",
  tokens: {
    "bg-base": "#ffffff", "surface-0": "#ffffff", "surface-1": "#fafafa",
    "surface-2": "#f5f5f5", "surface-3": "#e5e5e5", "term-bg": "#ffffff",
    "text-hi": "#171717", "text-mid": "#525252", "text-lo": "#737373", "term-fg": "#1f1f1f",
    "accent": "#171717", "on-accent": "#fafafa", "accent-container": "#f5f5f5", "on-accent-container": "#171717",
    "border-strong": "#d4d4d4", "border-subtle": "#e5e5e5",
    "status-ok": "#188038", "status-warn": "#b06000", "status-err": "#c5221f", "status-info": "#0b57d0",
    "syntax-plain": "#383a42", "syntax-keyword": "#a626a4", "syntax-string": "#50a14f", "syntax-number": "#c18401", "syntax-comment": "#a0a1a7",
    "secondary-container": "#f5f5f5", "on-secondary-container": "#171717",
    "wb-titlebar": "#fafafa", "wb-activity": "#fafafa", "wb-sidebar": "#fafafa",
    "wb-editor": "#ffffff", "wb-statusbar": "#fafafa", "wb-border": "#e5e5e5",
    "wb-active": "#171717", "wb-active-contrast": "#ffffff", "wb-focus": "#171717",
    "wb-selected": "#f5f5f5", "wb-selected-contrast": "#171717",
    "wb-tab-active": "#ffffff", "wb-tab-inactive": "#fafafa", "wb-tab-hover": "#f5f5f5",
    "wb-pane-drop": "#17171726", "wb-pane-focus": "#171717",
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
