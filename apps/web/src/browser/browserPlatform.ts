// Browser OS detection plus the one application-shortcut map. Terminal-facing
// components must use this module rather than treating Ctrl as Command: on
// Windows, plain Ctrl+letter belongs to the PTY and Ctrl+Alt may be AltGraph.

export type BrowserPlatform = "macos" | "windows" | "linux" | "other";

export interface BrowserNavigatorLike {
  userAgent?: string;
  platform?: string;
  userAgentData?: { platform?: string };
}

export interface ShortcutKeyEvent {
  key: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  getModifierState?: (key: string) => boolean;
}

export type PlatformShortcutId =
  | "commandPalette"
  | "sidebarSearch"
  | "toggleSidebar"
  | "settings"
  | "termFontIncrease"
  | "termFontDecrease"
  | "termFontReset"
  | "terminalTab"
  | "paneFocus"
  | "newTerminal"
  | "splitRight"
  | "splitDown"
  | "spotlight"
  | "arrangeBalance"
  | "arrangeGrid"
  | "arrangeColumns"
  | "arrangeRows"
  | "arrangeMain"
  | "terminalCopy"
  | "terminalPaste"
  | "terminalFind";

const WINDOWS_LABELS: Readonly<Record<PlatformShortcutId, string>> = {
  commandPalette: "Ctrl+Shift+P",
  sidebarSearch: "Ctrl+Shift+F",
  toggleSidebar: "Ctrl+Shift+B",
  settings: "Ctrl+,",
  termFontIncrease: "Ctrl++",
  termFontDecrease: "Ctrl+−",
  termFontReset: "Ctrl+0",
  terminalTab: "Alt+1–9",
  paneFocus: "Alt+← ↑ → ↓",
  newTerminal: "Ctrl+Shift+T",
  splitRight: "Alt+Shift+D",
  splitDown: "Alt+Shift+S",
  spotlight: "Alt+Enter",
  arrangeBalance: "Alt+Shift+B",
  arrangeGrid: "Alt+Shift+G",
  arrangeColumns: "Alt+Shift+E",
  arrangeRows: "Alt+Shift+R",
  arrangeMain: "Alt+Shift+V",
  terminalCopy: "Ctrl+Shift+C",
  terminalPaste: "Ctrl+Shift+V",
  terminalFind: "Ctrl+Shift+F",
};

export function detectBrowserPlatform(nav: BrowserNavigatorLike | undefined): BrowserPlatform {
  const hinted = nav?.userAgentData?.platform ?? nav?.platform ?? "";
  const ua = nav?.userAgent ?? "";
  if (/windows|win32|win64/i.test(`${hinted} ${ua}`)) return "windows";
  if (/macos|macintosh|macintel|iphone|ipad/i.test(`${hinted} ${ua}`)) return "macos";
  if (/linux|x11|cros|android/i.test(`${hinted} ${ua}`)) return "linux";
  return "other";
}

export function browserPlatform(): BrowserPlatform {
  return detectBrowserPlatform(typeof navigator === "undefined" ? undefined : navigator as BrowserNavigatorLike);
}

/** AltGraph is exposed inconsistently: Chromium normally reports modifier
 * state, while some Windows layouts surface the same physical key as Ctrl+Alt.
 * The Windows fallback is intentionally conservative—no app command is worth
 * eating a composed terminal character. */
export function isAltGraphEvent(
  event: ShortcutKeyEvent,
  platform: BrowserPlatform = browserPlatform(),
): boolean {
  if (event.key === "AltGraph") return true;
  try {
    if (event.getModifierState?.("AltGraph")) return true;
  } catch {
    // Synthetic/older event implementations may throw for unknown modifiers.
  }
  return platform === "windows" && event.ctrlKey && event.altKey && !event.metaKey;
}

function lowerKey(event: ShortcutKeyEvent): string {
  return event.key.length === 1 ? event.key.toLowerCase() : event.key;
}

function matchesWindowsShortcut(event: ShortcutKeyEvent, id: PlatformShortcutId): boolean {
  const key = lowerKey(event);
  const ctrlShiftLetter = (letter: string) =>
    key === letter && event.ctrlKey && event.shiftKey && !event.altKey && !event.metaKey;
  const altShiftLetter = (letter: string) =>
    key === letter && event.altKey && event.shiftKey && !event.ctrlKey && !event.metaKey;

  switch (id) {
    case "commandPalette": return ctrlShiftLetter("p");
    case "sidebarSearch": return ctrlShiftLetter("f");
    case "toggleSidebar": return ctrlShiftLetter("b");
    case "settings": return key === "," && event.ctrlKey && !event.shiftKey && !event.altKey && !event.metaKey;
    case "termFontIncrease":
      return (key === "+" || key === "=") && event.ctrlKey && !event.altKey && !event.metaKey;
    case "termFontDecrease":
      return (key === "-" || key === "_") && event.ctrlKey && !event.altKey && !event.metaKey;
    case "termFontReset": return key === "0" && event.ctrlKey && !event.altKey && !event.metaKey;
    case "terminalTab":
      return /^[1-9]$/.test(key) && event.altKey && !event.shiftKey && !event.ctrlKey && !event.metaKey;
    case "paneFocus":
      return /^Arrow(?:Left|Right|Up|Down)$/.test(key)
        && event.altKey && !event.shiftKey && !event.ctrlKey && !event.metaKey;
    case "newTerminal": return ctrlShiftLetter("t");
    case "splitRight": return altShiftLetter("d");
    case "splitDown": return altShiftLetter("s");
    case "spotlight":
      return key === "Enter" && event.altKey && !event.shiftKey && !event.ctrlKey && !event.metaKey;
    case "arrangeBalance": return altShiftLetter("b");
    case "arrangeGrid": return altShiftLetter("g");
    case "arrangeColumns": return altShiftLetter("e");
    case "arrangeRows": return altShiftLetter("r");
    case "arrangeMain": return altShiftLetter("v");
    case "terminalCopy": return ctrlShiftLetter("c");
    case "terminalPaste": return ctrlShiftLetter("v");
    case "terminalFind": return ctrlShiftLetter("f");
  }
}

function matchesExistingShortcut(event: ShortcutKeyEvent, id: PlatformShortcutId): boolean {
  const key = lowerKey(event);
  const eitherPrimary = event.metaKey || event.ctrlKey;
  switch (id) {
    case "commandPalette": return eitherPrimary && key === "k";
    case "sidebarSearch": return eitherPrimary && key === "f";
    case "toggleSidebar": return eitherPrimary && key === "b" && !event.shiftKey;
    case "settings": return eitherPrimary && key === "," && !event.altKey;
    case "termFontIncrease": return eitherPrimary && !event.altKey && (key === "+" || key === "=");
    case "termFontDecrease": return eitherPrimary && !event.altKey && (key === "-" || key === "_");
    case "termFontReset": return eitherPrimary && !event.altKey && key === "0";
    case "terminalTab": return eitherPrimary && !event.altKey && /^[1-9]$/.test(key);
    case "paneFocus":
      return eitherPrimary && event.altKey && /^Arrow(?:Left|Right|Up|Down)$/.test(key);
    case "newTerminal": return eitherPrimary && event.altKey && key === "t";
    case "splitRight":
      return event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey && key === "d";
    case "splitDown":
      return event.metaKey && !event.ctrlKey && !event.altKey && event.shiftKey && key === "d";
    case "spotlight":
      return event.metaKey && !event.ctrlKey && !event.altKey && key === "Enter";
    case "arrangeBalance": return event.metaKey && !event.ctrlKey && event.altKey && key === "b";
    case "arrangeGrid": return event.metaKey && !event.ctrlKey && event.altKey && key === "g";
    case "arrangeColumns": return event.metaKey && !event.ctrlKey && event.altKey && key === "e";
    case "arrangeRows": return event.metaKey && !event.ctrlKey && event.altKey && key === "r";
    case "arrangeMain": return event.metaKey && !event.ctrlKey && event.altKey && key === "v";
    case "terminalCopy":
      return eitherPrimary && event.shiftKey && !event.altKey && key === "c";
    case "terminalPaste":
      return eitherPrimary && event.shiftKey && !event.altKey && key === "v";
    case "terminalFind":
      return !event.altKey && key === "f"
        && ((event.metaKey && !event.ctrlKey && !event.shiftKey) || (event.ctrlKey && event.shiftKey));
  }
}

export function matchesPlatformShortcut(
  event: ShortcutKeyEvent,
  id: PlatformShortcutId,
  platform: BrowserPlatform = browserPlatform(),
): boolean {
  if (isAltGraphEvent(event, platform)) return false;
  return platform === "windows"
    ? matchesWindowsShortcut(event, id)
    : matchesExistingShortcut(event, id);
}

/** Keep every existing macOS/Linux label byte-for-byte; only Windows replaces
 * the caller's current label with its PTY-safe native binding. */
export function platformShortcutLabel(
  id: PlatformShortcutId,
  existingMacLinuxLabel: string,
  platform: BrowserPlatform = browserPlatform(),
): string {
  return platform === "windows" ? WINDOWS_LABELS[id] : existingMacLinuxLabel;
}

export function terminalLinkModifierKey(platform: BrowserPlatform = browserPlatform()): "Meta" | "Control" {
  return platform === "macos" ? "Meta" : "Control";
}
