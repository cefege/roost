// Reads rendered pane topology and persistence from the real browser surface.
// Portable-layout smoke specs use these assertions without duplicating DOM
// selectors or growing the end-to-end scenario past the repository size cap.

import type { Page } from "@playwright/test";
import { expect } from "./fixtures.ts";

export const LAYOUT_STORAGE_KEY = "roost.paneLayout.v1";

export type PaneSnapshot = {
  tabs: string[];
  selected: string | null;
  focused: boolean;
};

export type RenderedLayoutSnapshot = {
  multiPane: string | null;
  panes: PaneSnapshot[];
  dividerDirections: string[];
  visibleSessionIds: string[];
  focusedSessionIds: string[];
  pathname: string;
};

export type RuntimeIds = { paneIds: string[]; splitIds: string[] };

export async function readRenderedLayout(
  page: Page,
  sessionIds: readonly string[],
): Promise<RenderedLayoutSnapshot> {
  return page.evaluate((ids) => {
    const idFromTab = (tab: Element | null): string | null => {
      const testId = tab?.getAttribute("data-testid") ?? "";
      return testId.startsWith("tab-") ? testId.slice("tab-".length) : null;
    };
    const panes = Array.from(document.querySelectorAll<HTMLElement>("[data-pane-strip]"))
      .map((strip) => ({
        tabs: Array.from(strip.querySelectorAll(":scope > .workbench-pane-tab-strip__tabs > .df-tab"))
          .map((tab) => idFromTab(tab))
          .filter((id): id is string => id !== null),
        selected: idFromTab(strip.querySelector(":scope > .workbench-pane-tab-strip__tabs > .df-tab[data-active='true']")),
        focused: strip.getAttribute("data-focused") === "true",
      }));
    const slot = (sessionId: string) => document.querySelector<HTMLElement>(
      `[data-testid="terminal-slot-${CSS.escape(sessionId)}"]`,
    );
    const visibleSessionIds = ids.filter((sessionId) => {
      const element = slot(sessionId);
      if (!element || getComputedStyle(element).visibility === "hidden") return false;
      const rect = element.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0;
    });
    return {
      multiPane: document.querySelector("[data-testid='terminal-deck']")
        ?.getAttribute("data-multi-pane") ?? null,
      panes,
      dividerDirections: Array.from(
        document.querySelectorAll<HTMLElement>("[data-testid^='pane-divider-']"),
      ).map((divider) => divider.getAttribute("data-dir") ?? ""),
      visibleSessionIds,
      focusedSessionIds: ids.filter((sessionId) =>
        slot(sessionId)?.getAttribute("data-focused") === "true"),
      pathname: location.pathname,
    };
  }, [...sessionIds]);
}

export async function readRuntimeIds(
  page: Page,
  sessionIds: readonly string[],
): Promise<RuntimeIds> {
  return page.evaluate((ids) => ({
    paneIds: ids.map((sessionId) => document.querySelector(
      `[data-testid="terminal-slot-${CSS.escape(sessionId)}"]`,
    )?.getAttribute("data-pane-id")).filter((id): id is string => !!id),
    splitIds: Array.from(document.querySelectorAll<HTMLElement>(
      "[data-testid^='pane-divider-']",
    )).map((divider) => (divider.getAttribute("data-testid") ?? "")
      .slice("pane-divider-".length)),
  }), [...sessionIds]);
}

export async function waitForPersistedRuntimeIds(
  page: Page,
  runtimeIds: readonly string[],
): Promise<void> {
  await expect.poll(() => page.evaluate(({ ids, key }) => {
    const source = localStorage.getItem(key) ?? "";
    return ids.length > 0 && ids.every((runtimeId) => source.includes(runtimeId));
  }, { ids: [...runtimeIds], key: LAYOUT_STORAGE_KEY })).toBe(true);
}

export async function expectRenderedLayout(
  page: Page,
  sessionIds: readonly string[],
  expected: RenderedLayoutSnapshot,
): Promise<void> {
  await expect.poll(
    () => readRenderedLayout(page, sessionIds),
    { timeout: 30_000, intervals: [50, 100, 250] },
  ).toEqual(expected);
}
