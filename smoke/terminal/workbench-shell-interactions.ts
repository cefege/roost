// Workbench-shell interaction coverage shared by the desktop sidebar and compact drawer cases.
// It drives real coordinator-backed shells and agent reports through the established smoke fixtures.
// Geometry is read from rendered regions so split persistence never depends on browser-store internals.

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
const SIDEBAR_SPLIT_DEFAULT_PERCENT = 60;
const SIDEBAR_SPLIT_MIN_PERCENT = 20;
const SIDEBAR_SPLIT_MAX_PERCENT = 80;

type Rect = {
  top: number;
  bottom: number;
  height: number;
};

type ScrollState = {
  overflowY: string;
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
};

type SidebarSurface = {
  root: Rect;
  spaces: Rect;
  divider: Rect;
  agents: Rect;
  spacesScroll: ScrollState;
  agentsScroll: ScrollState;
};


async function readSidebarSurface(page: Page): Promise<SidebarSurface | null> {
  return page.evaluate(() => {
    const root = document.querySelector<HTMLElement>('[data-testid="sidebar-root"]');
    const spaces = document.querySelector<HTMLElement>('[data-testid="sidebar-spaces"]');
    const divider = document.querySelector<HTMLElement>('[data-testid="sidebar-section-resizer"]');
    const agents = document.querySelector<HTMLElement>('[data-testid="sidebar-agents-section"]');
    const spacesScroll = document.querySelector<HTMLElement>('[data-testid="all-view"]');
    const agentsScroll = document.querySelector<HTMLElement>('[data-testid="sidebar-agents"]');
    if (!root || !spaces || !divider || !agents || !spacesScroll || !agentsScroll) return null;
    const rect = (element: HTMLElement): Rect => {
      const box = element.getBoundingClientRect();
      return { top: box.top, bottom: box.bottom, height: box.height };
    };
    const scroll = (element: HTMLElement): ScrollState => ({
      overflowY: getComputedStyle(element).overflowY,
      scrollTop: element.scrollTop,
      scrollHeight: element.scrollHeight,
      clientHeight: element.clientHeight,
    });
    return {
      root: rect(root),
      spaces: rect(spaces),
      divider: rect(divider),
      agents: rect(agents),
      spacesScroll: scroll(spacesScroll),
      agentsScroll: scroll(agentsScroll),
    };
  });
}

async function expectVerticalSidebarSections(page: Page): Promise<SidebarSurface> {
  await expect.poll(() => readSidebarSurface(page), { timeout: 30_000 }).not.toBeNull();
  const surface = await readSidebarSurface(page);
  if (!surface) throw new Error("desktop sidebar did not expose both split sections");

  expect(surface.spaces.height).toBeGreaterThan(0);
  expect(surface.agents.height).toBeGreaterThan(0);
  expect(surface.spaces.top).toBeGreaterThanOrEqual(surface.root.top - 1);
  expect(surface.spaces.bottom).toBeLessThanOrEqual(surface.divider.top + 1);
  expect(surface.divider.bottom).toBeLessThanOrEqual(surface.agents.top + 1);
  expect(surface.agents.bottom).toBeLessThanOrEqual(surface.root.bottom + 1);
  return surface;
}

async function wheelWithinTestId(page: Page, testId: string): Promise<void> {
  const bounds = await page.getByTestId(testId).boundingBox();
  if (!bounds) throw new Error(`${testId} has no visible scroll surface`);
  await page.mouse.move(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
  await page.mouse.wheel(0, 1_000);
}

async function expectPanelScrolled(
  page: Page,
  panel: "spacesScroll" | "agentsScroll",
): Promise<SidebarSurface> {
  await expect.poll(async () => (await readSidebarSurface(page))?.[panel].scrollTop ?? 0, {
    timeout: 30_000,
  }).toBeGreaterThan(0);
  const surface = await readSidebarSurface(page);
  if (!surface) throw new Error("sidebar surface disappeared after scrolling");
  return surface;
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
    const folders = Array.from({ length: 14 }, (_value, index) => join(temporaryRoot, String(index)));
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
  const sidebar = page.getByTestId("sidebar-root");
  return Promise.all([
    expect(sidebar.getByText("Chat", { exact: true })).toHaveCount(0),
    expect(sidebar.locator('[data-testid*="chat"]')).toHaveCount(0),
  ]).then(() => undefined);
}

export async function exerciseSidebarAgents(
  page: Page,
  stack: TerminalTestStack,
  secondWorker: TerminalTestWorker,
): Promise<void> {
  await page.setViewportSize(WIDE_VIEWPORT);
  const { primarySessionId, agentSessionIds } = await spawnWorkingAgents(page, stack, secondWorker);
  const activeAgentSessionId = agentSessionIds[0];
  if (!activeAgentSessionId) throw new Error("agent fixture did not create an agent session");

  const activeAgentRow = page.getByTestId(`sidebar-agent-row-${activeAgentSessionId}`);
  await expect(activeAgentRow).toBeVisible({ timeout: 30_000 });
  await expectNoLegacyChatControls(page);

  const resizer = page.getByTestId("sidebar-section-resizer");
  await expect(resizer).toHaveAttribute("role", "separator");
  await expect(resizer).toHaveAttribute("aria-orientation", "horizontal");
  await expect(resizer).toHaveAttribute("aria-valuemin", String(SIDEBAR_SPLIT_MIN_PERCENT));
  await expect(resizer).toHaveAttribute("aria-valuemax", String(SIDEBAR_SPLIT_MAX_PERCENT));
  await expect(resizer).toHaveAttribute("aria-valuenow", String(SIDEBAR_SPLIT_DEFAULT_PERCENT));

  const [rootBounds, dividerBounds] = await Promise.all([
    page.getByTestId("sidebar-root").boundingBox(),
    resizer.boundingBox(),
  ]);
  if (!rootBounds || !dividerBounds) throw new Error("sidebar split has no pointer bounds");
  const dividerX = dividerBounds.x + dividerBounds.width / 2;
  const dividerY = dividerBounds.y + dividerBounds.height / 2;
  await resizer.hover();
  await page.mouse.down();
  await page.mouse.move(dividerX, dividerY + rootBounds.height * 0.1, { steps: 8 });
  await page.mouse.up();
  await expect(resizer).not.toHaveAttribute("aria-valuenow", String(SIDEBAR_SPLIT_DEFAULT_PERCENT));
  const draggedRatio = await resizer.getAttribute("aria-valuenow");
  if (!draggedRatio) throw new Error("sidebar split drag did not publish a ratio");
  await expectVerticalSidebarSections(page);

  await page.reload({ waitUntil: "domcontentloaded" });
  await navigateToSmokeSession(page, primarySessionId);
  const reloadedResizer = page.getByTestId("sidebar-section-resizer");
  await expect(reloadedResizer).toHaveAttribute("aria-valuenow", draggedRatio);
  await reloadedResizer.focus();
  await page.keyboard.press("Home");
  await expect(reloadedResizer).toHaveAttribute("aria-valuenow", String(SIDEBAR_SPLIT_MIN_PERCENT));

  await page.getByTestId("brand-row-search").click();
  await page.getByTestId("sidebar-search").fill("/tmp");
  await expect.poll(() => page.getByTestId("sidebar-session-row").count(), {
    timeout: 30_000,
  }).toBeGreaterThanOrEqual(agentSessionIds.length + 1);
  const spacesBeforeScroll = await readSidebarSurface(page);
  if (!spacesBeforeScroll) throw new Error("Spaces scroll surface is unavailable");
  expect(spacesBeforeScroll.spacesScroll.overflowY).toBe("auto");
  expect(spacesBeforeScroll.spacesScroll.scrollHeight).toBeGreaterThan(
    spacesBeforeScroll.spacesScroll.clientHeight,
  );
  await wheelWithinTestId(page, "all-view");
  const spacesAfterScroll = await expectPanelScrolled(page, "spacesScroll");
  expect(spacesAfterScroll.agentsScroll.scrollTop).toBe(spacesBeforeScroll.agentsScroll.scrollTop);
  await reloadedResizer.focus();
  await page.keyboard.press("Home");
  await expect(reloadedResizer).toHaveAttribute("aria-valuenow", String(SIDEBAR_SPLIT_MIN_PERCENT));
  await page.keyboard.press("ArrowDown");
  await expect(reloadedResizer).toHaveAttribute("aria-valuenow", "25");
  await page.keyboard.press("ArrowUp");
  await expect(reloadedResizer).toHaveAttribute("aria-valuenow", String(SIDEBAR_SPLIT_MIN_PERCENT));

  await reloadedResizer.focus();
  await page.keyboard.press("End");
  await expect(reloadedResizer).toHaveAttribute("aria-valuenow", String(SIDEBAR_SPLIT_MAX_PERCENT));
  const agentsBeforeScroll = await expectVerticalSidebarSections(page);
  expect(agentsBeforeScroll.agentsScroll.overflowY).toBe("auto");
  expect(agentsBeforeScroll.agentsScroll.scrollHeight).toBeGreaterThan(
    agentsBeforeScroll.agentsScroll.clientHeight,
  );
  await wheelWithinTestId(page, "sidebar-agents");
  const agentsAfterScroll = await expectPanelScrolled(page, "agentsScroll");
  expect(agentsAfterScroll.spacesScroll.scrollTop).toBe(agentsBeforeScroll.spacesScroll.scrollTop);

  await reloadedResizer.dblclick();
  await expect(reloadedResizer).toHaveAttribute("aria-valuenow", String(SIDEBAR_SPLIT_DEFAULT_PERCENT));
  await page.reload({ waitUntil: "domcontentloaded" });
  await navigateToSmokeSession(page, primarySessionId);
  await expect(page.getByTestId("sidebar-section-resizer"))
    .toHaveAttribute("aria-valuenow", String(SIDEBAR_SPLIT_DEFAULT_PERCENT));

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
