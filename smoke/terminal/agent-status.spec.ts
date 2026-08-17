// Coding-agent status path, end to end without model credentials:
// spawned PTY env -> worker UDS report server -> worker/coord protobuf -> Sync
// -> session/tab/folder state, delayed toast, title ACK, and inactive cleanup.

import { test, expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";

interface AgentStatusSmoke {
  spawnShell(worker: string, folder: string): Promise<{ session_id: string }>;
  input(sessionId: string, text: string): Promise<void>;
  forceVisible(on: boolean): void;
}

function reportCommand(
  state: "working" | "blocked" | "idle",
  seq: number,
  active = true,
  message?: string,
): string {
  const messageField = message === undefined ? "" : `,"message":${JSON.stringify(message)}`;
  const python = [
    "import json,os,socket",
    "s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)",
    "s.connect(os.environ['ROOST_AGENT_SOCKET_PATH'])",
    `p={"version":1,"method":"agent.report","capability":os.environ["ROOST_AGENT_CAPABILITY"],"params":{"session_id":os.environ["ROOST_SESSION_ID"],"pid":os.getpid(),"agent":"omp","state":"${state}","seq":${seq},"active":${active ? "True" : "False"}${messageField}}}`,
    "s.sendall((json.dumps(p)+'\\n').encode())",
    "r=s.recv(4096)",
    `print('STATUS_ACK_${seq}',r.decode().strip())`,
  ].join(";");
  return `python3 -c ${JSON.stringify(python)}\r`;
}

async function report(
  page: Page,
  sessionId: string,
  state: "working" | "blocked" | "idle",
  seq: number,
  active = true,
  message?: string,
): Promise<void> {
  await page.evaluate(async ({ id, command }) => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    await smoke.input(id, command);
  }, { id: sessionId, command: reportCommand(state, seq, active, message) });
}

test("agent status reaches every browser surface and notification ACK", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop status surfaces");
  test.setTimeout(180_000);

  const [activeId, backgroundId] = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    const first = await smoke.spawnShell(workerFp, "/tmp");
    const second = await smoke.spawnShell(workerFp, "/tmp");
    smoke.forceVisible(true);
    return [first.session_id, second.session_id];
  }, stack.workerFp);

  await smokePage.goto(`${stack.baseUrl}/s/${activeId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${activeId}`)).toBeVisible();
  await expect(smokePage.getByTestId(`tab-${backgroundId}`)).toBeVisible();

  const tabStatus = smokePage
    .getByTestId(`tab-${backgroundId}`)
    .getByTestId(`agent-status-${backgroundId}`);
  const folderStatus = smokePage.locator('[data-testid^="folder-agent-status-"]').first();

  // One report is checked for its socket ACK so a silently rejected report
  // (bad pid ownership, unwritable socket) can't masquerade as a UI bug; the
  // remaining reports are proven by the surfaces they move.
  await report(smokePage, backgroundId, "working", 1);
  await smokePage.goto(`${stack.baseUrl}/s/${backgroundId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${backgroundId}`)).toContainText(
    `STATUS_ACK_1 {"ok":true}`,
    { timeout: 30_000 },
  );
  await smokePage.goto(`${stack.baseUrl}/s/${activeId}`);
  await expect(tabStatus).toHaveAttribute("data-level", "working", { timeout: 30_000 });
  await expect(folderStatus).toHaveAttribute("data-level", "working");
  await expect(folderStatus).toContainText("1 working");

  // Search switches the sidebar from folder rows to the full per-session row
  // without changing the active terminal. This locks the distinct session-row
  // surface as well as the folder rollup above.
  await smokePage.getByTestId("brand-row-search").click();
  await smokePage.getByTestId("sidebar-search").fill("/tmp");
  const sessionRow = smokePage.locator(
    `[data-testid="sidebar-session-row"][data-session-id="${backgroundId}"]`,
  );
  await expect(sessionRow).toBeVisible();
  await expect(sessionRow.getByTestId(`agent-status-${backgroundId}`)).toHaveAttribute(
    "data-level",
    "working",
  );
  await smokePage.getByTestId("sidebar-search-clear").click();
  await expect(folderStatus).toBeVisible();

  await report(smokePage, backgroundId, "blocked", 2, true, "Approval needed");
  await expect(tabStatus).toHaveAttribute("data-level", "blocked", { timeout: 30_000 });
  await expect(folderStatus).toHaveAttribute("data-level", "blocked");
  await expect(folderStatus).toContainText("1 needs input");
  await expect.poll(() => smokePage.title()).toMatch(/^\(1\) Roost/);
  const blockedToast = smokePage.getByTestId("toast").filter({ hasText: "needs your input" });
  await expect(blockedToast).toBeVisible({ timeout: 10_000 });
  await expect(blockedToast.getByTestId("toast-details")).toContainText("Approval needed");

  await blockedToast.getByRole("button", { name: "View" }).click();
  await expect(smokePage.getByTestId(`tab-${backgroundId}`)).toHaveAttribute("data-active", "true");
  await expect.poll(() => smokePage.title()).not.toMatch(/^\(\d+\)/);

  // Move away before completion so the transition is genuinely backgrounded.
  await smokePage.goto(`${stack.baseUrl}/s/${activeId}`);
  await expect(smokePage.getByTestId(`tab-${activeId}`)).toHaveAttribute("data-active", "true");
  await report(smokePage, backgroundId, "working", 3);
  await expect(tabStatus).toHaveAttribute("data-level", "working", { timeout: 30_000 });
  await report(smokePage, backgroundId, "idle", 4);
  await expect(tabStatus).toHaveAttribute("data-level", "done", { timeout: 30_000 });
  await expect(folderStatus).toHaveAttribute("data-level", "done");
  await expect(folderStatus).toContainText("1 done");
  const doneToast = smokePage.getByTestId("toast").filter({ hasText: "finished" });
  await expect(doneToast).toBeVisible({ timeout: 10_000 });
  await expect.poll(() => smokePage.title()).toMatch(/^\(1\) Roost/);

  await doneToast.getByRole("button", { name: "View" }).click();
  await expect(tabStatus).toHaveAttribute("data-level", "idle");
  await expect.poll(() => smokePage.title()).not.toMatch(/^\(\d+\)/);

  await report(smokePage, backgroundId, "idle", 5, false);
  await expect(tabStatus).toHaveCount(0, { timeout: 30_000 });
  await expect(folderStatus).toHaveCount(0);
});
