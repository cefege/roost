import { describe, expect, test } from "bun:test";
import {
  detectBrowserPlatform,
  isAltGraphEvent,
  matchesPlatformShortcut,
  platformShortcutLabel,
  type ShortcutKeyEvent,
} from "../src/lib/browserPlatform.ts";

function key(
  value: string,
  mods: Partial<Omit<ShortcutKeyEvent, "key">> = {},
): ShortcutKeyEvent {
  return {
    key: value,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    metaKey: false,
    ...mods,
  };
}

describe("browser platform shortcuts", () => {
  test("detects UA-CH first with UA fallbacks", () => {
    expect(detectBrowserPlatform({ userAgentData: { platform: "Windows" } })).toBe("windows");
    expect(detectBrowserPlatform({ platform: "MacIntel" })).toBe("macos");
    expect(detectBrowserPlatform({ userAgent: "Mozilla/5.0 (X11; Linux x86_64)" })).toBe("linux");
  });

  test("preserves existing macOS/Linux labels", () => {
    expect(platformShortcutLabel("commandPalette", "⌘K", "macos")).toBe("⌘K");
    expect(platformShortcutLabel("commandPalette", "⌘K", "linux")).toBe("⌘K");
    expect(platformShortcutLabel("commandPalette", "⌘K", "windows")).toBe("Ctrl+Shift+P");
  });

  test("preserves current macOS and Linux command behavior", () => {
    expect(matchesPlatformShortcut(key("k", { metaKey: true }), "commandPalette", "macos")).toBe(true);
    expect(matchesPlatformShortcut(key("k", { ctrlKey: true }), "commandPalette", "linux")).toBe(true);
    expect(matchesPlatformShortcut(key("b", { ctrlKey: true }), "toggleSidebar", "linux")).toBe(true);
  });

  test("Windows never binds plain Ctrl letters", () => {
    for (const [letter, shortcut] of [
      ["k", "commandPalette"],
      ["f", "sidebarSearch"],
      ["b", "toggleSidebar"],
      ["t", "newTerminal"],
    ] as const) {
      expect(matchesPlatformShortcut(key(letter, { ctrlKey: true }), shortcut, "windows")).toBe(false);
    }
    expect(matchesPlatformShortcut(key("p", { ctrlKey: true, shiftKey: true }), "commandPalette", "windows")).toBe(true);
    expect(matchesPlatformShortcut(key("t", { ctrlKey: true, shiftKey: true }), "newTerminal", "windows")).toBe(true);
  });

  test("AltGraph and its Windows Ctrl+Alt representation never trigger app shortcuts", () => {
    const represented = key("t", { ctrlKey: true, altKey: true });
    expect(isAltGraphEvent(represented, "windows")).toBe(true);
    expect(matchesPlatformShortcut(represented, "newTerminal", "windows")).toBe(false);

    const explicit = key("g", {
      ctrlKey: true,
      altKey: true,
      getModifierState: (name) => name === "AltGraph",
    });
    expect(isAltGraphEvent(explicit, "linux")).toBe(true);
    expect(matchesPlatformShortcut(explicit, "arrangeGrid", "linux")).toBe(false);
  });

  test("Windows pane navigation and tab selection avoid Control", () => {
    expect(matchesPlatformShortcut(key("ArrowLeft", { altKey: true }), "paneFocus", "windows")).toBe(true);
    expect(matchesPlatformShortcut(key("3", { altKey: true }), "terminalTab", "windows")).toBe(true);
    expect(matchesPlatformShortcut(key("ArrowLeft", { ctrlKey: true, altKey: true }), "paneFocus", "windows")).toBe(false);
  });
});
