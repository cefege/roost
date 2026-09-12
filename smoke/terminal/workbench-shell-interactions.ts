// Workbench-shell selector coverage shared by the desktop sidebar and compact drawer cases.
// It drives real coordinator-backed shells and agent reports through the established smoke fixtures.
// Each list remains mounted so native scroll retention is observable without synthetic sidebar state.

import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Page } from "@playwright/test";
import { expect } from "./fixtures.ts";
import {
  launchIntegratedAgent,
  pollAgentStatus,
  reportAgentStatus,
} from "./agent-status-fixture.ts";
import { navigateToSmokeSession, spawnSmokeShell } from "./terminal-helpers.ts";
import type { TerminalTestStack, TerminalTestWorker } from "./stack.ts";

const WIDE_VIEWPORT = { width: 1440, height: 900 } as const;
const AGENT_SESSION_COUNT = 14;

type SidebarView = "spaces" | "agents";
type SidebarEdge = "top" | "bottom";

type ScrollState = {
  overflowY: string;
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
};
type SidebarPanelState = { ariaHidden: string | null; inert: boolean; scroll: ScrollState };
type SidebarSurface = { spaces: SidebarPanelState; agents: SidebarPanelState };

function sidebarPanelTestId(view: SidebarView): string {
  return view === "spaces" ? "sidebar-spaces" : "sidebar-agents-section";
}
function sidebarScrollTestId(view: SidebarView): string {
  return view === "spaces" ? "all-view" : "sidebar-agents";
}
function sidebarViewButton(page: Page, view: SidebarView) {
  return page.getByTestId(`sidebar-view-${view}`);
}
async function readSidebarSurface(page: Page): Promise<SidebarSurface | null> {
  return page.evaluate(() => {
    const spacesPanel = document.querySelector<HTMLElement>('[data-testid="sidebar-spaces"]');
    const agentsPanel = document.querySelector<HTMLElement>(
      '[data-testid="sidebar-agents-section"]',
    );
    const spacesScroll = document.querySelector<HTMLElement>('[data-testid="all-view"]');
    const agentsScroll = document.querySelector<HTMLElement>('[data-testid="sidebar-agents"]');
    if (!spacesPanel || !agentsPanel || !spacesScroll || !agentsScroll) return null;

    const readScroll = (element: HTMLElement): ScrollState => ({
      overflowY: getComputedStyle(element).overflowY,
      scrollTop: element.scrollTop,
      scrollHeight: element.scrollHeight,
      clientHeight: element.clientHeight,
    });
    const readPanel = (panel: HTMLElement, scrollHost: HTMLElement): SidebarPanelState => ({
      ariaHidden: panel.getAttribute("aria-hidden"),
      inert: panel.inert,
      scroll: readScroll(scrollHost),
    });
    return {
      spaces: readPanel(spacesPanel, spacesScroll),
      agents: readPanel(agentsPanel, agentsScroll),
    };
  });
}
async function expectSidebarHosts(page: Page): Promise<SidebarSurface> {
  await expect(page.getByTestId("sidebar-spaces")).toHaveCount(1);
  await expect(page.getByTestId("sidebar-agents-section")).toHaveCount(1);
  await expect(page.getByTestId("all-view")).toHaveCount(1);
  await expect(page.getByTestId("sidebar-agents")).toHaveCount(1);
  await expect.poll(() => readSidebarSurface(page), { timeout: 30_000 }).not.toBeNull();
  const surface = await readSidebarSurface(page);
  if (!surface) throw new Error("sidebar selector did not retain both list hosts");

  for (const panel of [surface.spaces, surface.agents]) {
    expect(panel.scroll.overflowY).toBe("auto");
    expect(panel.scroll.clientHeight).toBeGreaterThan(0);
  }
  return surface;
}
async function expectSidebarView(page: Page, selectedView: SidebarView): Promise<void> {
  const inactiveView: SidebarView = selectedView === "spaces" ? "agents" : "spaces";
  await expect(sidebarViewButton(page, selectedView)).toHaveAttribute("aria-pressed", "true");
  await expect(sidebarViewButton(page, inactiveView)).toHaveAttribute("aria-pressed", "false");
  await expect(page.getByTestId(sidebarPanelTestId(inactiveView))).toHaveAttribute("aria-hidden", "true");

  const surface = await expectSidebarHosts(page);
  expect(surface[selectedView].ariaHidden).not.toBe("true");
  expect(surface[inactiveView].ariaHidden).toBe("true");
  expect(surface[inactiveView].inert).toBe(true);
}
async function selectSidebarView(page: Page, view: SidebarView): Promise<void> {
  await sidebarViewButton(page, view).click();
  await expectSidebarView(page, view);
}

async function expectInactivePanelSkippedByTab(page: Page, activeView: SidebarView): Promise<void> {
  const inactiveView: SidebarView = activeView === "spaces" ? "agents" : "spaces";
  await sidebarViewButton(page, "agents").focus();
  await page.keyboard.press("Tab");
  const focusState = await page.evaluate(({ activePanelTestId, inactivePanelTestId }) => {
    const activePanel = document.querySelector<HTMLElement>(
      `[data-testid="${activePanelTestId}"]`,
    );
    const inactivePanel = document.querySelector<HTMLElement>(
      `[data-testid="${inactivePanelTestId}"]`,
    );
    const focused = document.activeElement;
    return {
      inActivePanel: !!focused && !!activePanel?.contains(focused),
      inInactivePanel: !!focused && !!inactivePanel?.contains(focused),
    };
  }, {
    activePanelTestId: sidebarPanelTestId(activeView),
    inactivePanelTestId: sidebarPanelTestId(inactiveView),
  });
  expect(focusState.inInactivePanel).toBe(false);
  expect(focusState.inActivePanel).toBe(true);
}

async function wheelWithinTestId(page: Page, testId: string, deltaY: number): Promise<void> {
  const bounds = await page.getByTestId(testId).boundingBox();
  if (!bounds) throw new Error(`${testId} has no visible scroll surface`);
  await page.mouse.move(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
  await page.mouse.wheel(0, deltaY);
}

async function setPanelScrollEdge(page: Page, view: SidebarView, edge: SidebarEdge): Promise<void> {
  await page.getByTestId(sidebarScrollTestId(view)).evaluate((element, targetEdge) => {
    const scrollHost = element as HTMLElement;
    scrollHost.scrollTop = targetEdge === "top"
      ? 0
      : Math.max(0, scrollHost.scrollHeight - scrollHost.clientHeight);
  }, edge);
  await expectPanelAtEdge(page, view, edge);
}

async function expectPanelAtEdge(page: Page, view: SidebarView, edge: SidebarEdge): Promise<void> {
  await expect.poll(async () => {
    const surface = await readSidebarSurface(page);
    if (!surface) return Number.POSITIVE_INFINITY;
    const scroll = surface[view].scroll;
    const expectedScrollTop = edge === "top"
      ? 0
      : Math.max(0, scroll.scrollHeight - scroll.clientHeight);
    return Math.abs(scroll.scrollTop - expectedScrollTop);
  }, { timeout: 30_000 }).toBeLessThanOrEqual(1);
}

async function dispatchTouchScroll(page: Page, testId: string, deltaY: number): Promise<void> {
  await page.evaluate(({ scrollTestId, movementY }) => {
    const scrollHost = document.querySelector<HTMLElement>(`[data-testid="${scrollTestId}"]`);
    if (!scrollHost) throw new Error(`${scrollTestId} did not mount`);
    const bounds = scrollHost.getBoundingClientRect();
    const startX = bounds.left + bounds.width / 2;
    const startY = bounds.top + bounds.height / 2;
    const touchAt = (clientY: number) => new Touch({
      identifier: 1,
      target: scrollHost,
      clientX: startX,
      clientY,
      screenX: startX,
      screenY: clientY,
      pageX: startX,
      pageY: clientY,
    });
    const initialTouch = touchAt(startY);
    const movedTouch = touchAt(startY + movementY);
    scrollHost.dispatchEvent(new TouchEvent("touchstart", {
      bubbles: true,
      cancelable: true,
      changedTouches: [initialTouch],
      touches: [initialTouch],
    }));
    scrollHost.dispatchEvent(new TouchEvent("touchmove", {
      bubbles: true,
      cancelable: true,
      changedTouches: [movedTouch],
      touches: [movedTouch],
    }));
    scrollHost.dispatchEvent(new TouchEvent("touchend", {
      bubbles: true,
      cancelable: true,
      changedTouches: [movedTouch],
      touches: [],
    }));
  }, { scrollTestId: testId, movementY: deltaY });
}

async function expectEdgeGesturesPreserveView(
  page: Page,
  view: SidebarView,
  edge: SidebarEdge,
): Promise<void> {
  await selectSidebarView(page, view);
  await setPanelScrollEdge(page, view, edge);
  const overscrollDelta = edge === "top" ? -120 : 120;
  await wheelWithinTestId(page, sidebarScrollTestId(view), overscrollDelta);
  await expectSidebarView(page, view);
  await dispatchTouchScroll(page, sidebarScrollTestId(view), edge === "top" ? 80 : -80);
  await expectSidebarView(page, view);
  await expectPanelAtEdge(page, view, edge);
}

async function scrollPanel(page: Page, view: SidebarView, deltaY: number): Promise<number> {
  await wheelWithinTestId(page, sidebarScrollTestId(view), deltaY);
  await expect.poll(async () => (await readSidebarSurface(page))?.[view].scroll.scrollTop ?? 0, {
    timeout: 30_000,
  }).toBeGreaterThan(0);
  const surface = await readSidebarSurface(page);
  if (!surface) throw new Error("sidebar surface disappeared after scrolling");
  return surface[view].scroll.scrollTop;
}

async function expectRetainedPanelPosition(
  page: Page,
  view: SidebarView,
  expectedScrollTop: number,
): Promise<void> {
  await expect.poll(async () => {
    const surface = await readSidebarSurface(page);
    return Math.abs((surface?.[view].scroll.scrollTop ?? Number.POSITIVE_INFINITY) - expectedScrollTop);
  }, { timeout: 30_000 }).toBeLessThanOrEqual(1);
}

async function spawnWorkingAgents(
  page: Page,
  stack: TerminalTestStack,
  secondWorker: TerminalTestWorker,
): Promise<{ primarySessionId: string; agentSessionIds: readonly string[] }> {
  const primarySessionId = (await spawnSmokeShell(page, stack.workerFp)).session_id;
  const agentSessionIds: string[] = [];
  const temporaryRoot = await mkdtemp(join(tmpdir(), "roost-sidebar-smoke-"));
  try {
    const folders = Array.from(
      { length: AGENT_SESSION_COUNT },
      (_value, index) => join(temporaryRoot, String(index)),
    );
    await Promise.all(folders.map((folder) => mkdir(folder, { recursive: true })));
    for (const [index, folder] of folders.entries()) {
      const workerFp = index % 2 === 0 ? secondWorker.workerFp : stack.workerFp;
      const shell = await page.evaluate(
        ({ fp, cwd }) => window.__smoke.spawnShell(fp, cwd),
        { fp: workerFp, cwd: folder },
      );
      agentSessionIds.push(shell.session_id);
    }
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }

  await navigateToSmokeSession(page, primarySessionId);
  for (const sessionId of agentSessionIds) {
    await navigateToSmokeSession(page, sessionId);
    await launchIntegratedAgent(page, sessionId);
    await pollAgentStatus(
      stack.client,
      sessionId,
      "idle",
      (status) => status.source === "screen",
    );
    await reportAgentStatus(sessionId, "working", 1);
    await expect(page.getByTestId(`terminal-slot-${sessionId}`)).toContainText(
      'STATUS_ACK_1 {"ok":true}',
      { timeout: 30_000 },
    );
    await pollAgentStatus(
      stack.client,
      sessionId,
      "working",
      (status) => status.source === "integration",
    );
  }
  await navigateToSmokeSession(page, primarySessionId);
  return { primarySessionId, agentSessionIds };
}

function expectNoLegacyChatControls(page: Page): Promise<void> {
  const sidebarPanels = page.locator(
    '[data-testid="sidebar-spaces"], [data-testid="sidebar-agents-section"]',
  );
  return Promise.all([
    expect(sidebarPanels.getByText("Chat", { exact: true })).toHaveCount(0),
    expect(sidebarPanels.locator('[data-testid*="chat"]')).toHaveCount(0),
  ]).then(() => undefined);
}

export async function exerciseSidebarAgents(
  page: Page,
  stack: TerminalTestStack,
  secondWorker: TerminalTestWorker,
): Promise<void> {
  await page.setViewportSize(WIDE_VIEWPORT);
  const selector = page.getByRole("group", { name: "Sidebar view" });
  await expect(selector).toBeVisible();
  await expect(selector.getByRole("button", { name: "Spaces", exact: true })).toHaveCount(1);
  await expect(selector.getByRole("button", { name: "Agents", exact: true })).toHaveCount(1);
  await expect(page.getByRole("tablist", { name: "Sidebar view" })).toHaveCount(0);
  await expectSidebarView(page, "spaces");

  await selectSidebarView(page, "agents");
  await expect(page.getByTestId("sidebar-agents")).toContainText("No active agents");
  await expectSidebarView(page, "agents");

  const { primarySessionId, agentSessionIds } = await spawnWorkingAgents(page, stack, secondWorker);
  const activeAgentSessionId = agentSessionIds[0];
  if (!activeAgentSessionId) throw new Error("agent fixture did not create an agent session");
  expect(agentSessionIds).toHaveLength(AGENT_SESSION_COUNT);

  const activeAgentRow = page.getByTestId(`sidebar-agent-row-${activeAgentSessionId}`);
  await expect(activeAgentRow).toBeVisible({ timeout: 30_000 });
  await expect(page.locator('[data-testid^="sidebar-agent-row-"]')).toHaveCount(AGENT_SESSION_COUNT);
  await expectNoLegacyChatControls(page);
  await expectSidebarView(page, "agents");
  await expectInactivePanelSkippedByTab(page, "agents");

  await selectSidebarView(page, "spaces");
  await expectInactivePanelSkippedByTab(page, "spaces");
  const sidebarSearch = page.getByTestId("sidebar-search");
  await expect(sidebarSearch).toBeVisible();
  await sidebarSearch.fill("/tmp");
  await expect.poll(() => page.getByTestId("sidebar-session-row").count(), {
    timeout: 30_000,
  }).toBeGreaterThanOrEqual(agentSessionIds.length + 1);

  const overflowingPanels = await expectSidebarHosts(page);
  expect(overflowingPanels.spaces.scroll.scrollHeight).toBeGreaterThan(
    overflowingPanels.spaces.scroll.clientHeight,
  );
  expect(overflowingPanels.agents.scroll.scrollHeight).toBeGreaterThan(
    overflowingPanels.agents.scroll.clientHeight,
  );

  await expectEdgeGesturesPreserveView(page, "spaces", "top");
  await expectEdgeGesturesPreserveView(page, "spaces", "bottom");
  await expectEdgeGesturesPreserveView(page, "agents", "top");
  await expectEdgeGesturesPreserveView(page, "agents", "bottom");

  await selectSidebarView(page, "spaces");
  await setPanelScrollEdge(page, "spaces", "top");
  const spacesScrollTop = await scrollPanel(page, "spaces", 96);
  await selectSidebarView(page, "agents");
  await setPanelScrollEdge(page, "agents", "top");
  const agentsScrollTop = await scrollPanel(page, "agents", 288);
  expect(Math.abs(spacesScrollTop - agentsScrollTop)).toBeGreaterThan(1);

  for (let index = 0; index < 5; index += 1) {
    await selectSidebarView(page, "spaces");
    await expectRetainedPanelPosition(page, "spaces", spacesScrollTop);
    await selectSidebarView(page, "agents");
    await expectRetainedPanelPosition(page, "agents", agentsScrollTop);
  }

  await page.reload({ waitUntil: "domcontentloaded" });
  await navigateToSmokeSession(page, primarySessionId);
  await expectSidebarView(page, "agents");
  await expect(activeAgentRow).toBeVisible({ timeout: 30_000 });
  await activeAgentRow.click();
  await expect(page).toHaveURL(`${stack.baseUrl}/s/${activeAgentSessionId}`);
  await expect(page.getByTestId(`terminal-slot-${activeAgentSessionId}`)).toBeVisible();
  await expect(activeAgentRow).toHaveAttribute("data-selected", "true");
}

export async function swipeFromEdge(page: Page, startX: number, endX: number, y: number): Promise<void> {
  await page.evaluate(({ startX: initialX, endX: finalX, y: clientY }) => {
    const target = document.body;
    const touch = (clientX: number) => new Touch({
      identifier: 1,
      target,
      clientX,
      clientY,
      screenX: clientX,
      screenY: clientY,
      pageX: clientX,
      pageY: clientY,
    });
    target.dispatchEvent(new TouchEvent("touchstart", {
      bubbles: true,
      cancelable: true,
      changedTouches: [touch(initialX)],
      touches: [touch(initialX)],
    }));
    target.dispatchEvent(new TouchEvent("touchmove", {
      bubbles: true,
      cancelable: true,
      changedTouches: [touch(finalX)],
      touches: [touch(finalX)],
    }));
    target.dispatchEvent(new TouchEvent("touchend", {
      bubbles: true,
      cancelable: true,
      changedTouches: [touch(finalX)],
      touches: [],
    }));
  }, { startX, endX, y });
}
