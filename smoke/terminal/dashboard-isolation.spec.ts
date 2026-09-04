import { writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Page } from "@playwright/test";
import { test, expect } from "./fixtures.ts";
import { encodePtyFixtureCommand, PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import {
  inputSmokeTerminal,
  navigateToSmokeSession,
  spawnPtyFixtureSession,
} from "./terminal-helpers.ts";
import { waitForPainted } from "./terminal-multiview-helpers.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import type { TerminalTestStack } from "./stack.ts";

type DashboardSmokeApi = RecoverySmokeApi & {
  kill(sessionId: string): Promise<{ accepted: boolean }>;
  attachmentProbe(
    sessionId: string,
    sha256: string,
    sizeBytes: number,
    filename?: string,
  ): Promise<{ hit: boolean; abs_path: string }>;
  downloadWorkerFile(workerFp: string, path: string): Promise<{ bytes: number; sha256: string }>;
};

declare global {
  interface Window {
    __smoke: DashboardSmokeApi;
  }
}


async function waitForScopedWorker(page: Page, workerFp: string): Promise<void> {
  await page.waitForFunction((fp) => {
    const smoke = window.__smoke;
    return !!smoke.state().workers[fp];
  }, workerFp);
}
async function expectDashboardSelection(page: Page, dashboardId: string): Promise<void> {
  await expect.poll(() => page.getByTestId("dashboard-selector").evaluate((element) =>
    "value" in element ? String(element.value) : ""
  )).toBe(dashboardId);
}


async function expectNoDashboardLeak(
  page: Page,
  sessionId: string,
  workerFp: string,
  marker: string,
): Promise<void> {
  // A passing point-in-time assertion could precede a delayed live frame. Keep
  // observing the browser's projected store and rendered surface after B has
  // painted the marker, rather than inspecting the coordinator internals.
  const deadline = await page.evaluate(() => Date.now() + 750);
  await expect.poll(async () => page.evaluate(({ deadlineMs, id, fp, text }) => {
    const smoke = window.__smoke;
    const state = smoke.state();
    return Date.now() >= deadlineMs
      && !state.sessions[id]
      && !state.workers[fp]
      && !document.body.textContent?.includes(text)
      && !smoke.viewportText(id).includes(text);
  }, { deadlineMs: deadline, id: sessionId, fp: workerFp, text: marker }), {
    timeout: 4_000,
    intervals: [50, 100, 250],
  }).toBe(true);
}

async function expectNotFound(operation: () => Promise<unknown>): Promise<void> {
  try {
    await operation();
  } catch (error) {
    if (error instanceof Error && /not found/i.test(error.message)) return;
    throw new Error(`expected not-found denial, received ${String(error)}`);
  }
  throw new Error("expected not-found denial, but the operation succeeded");
}

async function scopedSession(
  client: TerminalTestStack["client"],
  sessionId: string,
) {
  const { sessions } = await client.sessionsList({ status: "all" });
  const session = sessions.find((item) => item.id === sessionId);
  if (!session) throw new Error(`expected scoped session ${sessionId}`);
  return session;
}

test("dashboard A cannot observe or mutate dashboard B terminal resources", async ({
  smokePage: dashboardAPage,
  secondSmokePage: dashboardACollaborator,
  secondDashboardSmokePage: dashboardBPage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop dashboard-isolation release gate");
  test.setTimeout(240_000);

  expect(dashboardAPage.context()).not.toBe(dashboardACollaborator.context());
  expect(dashboardAPage.context()).not.toBe(dashboardBPage.context());
  await Promise.all([
    dashboardAPage.evaluate(() => {
      window.__smoke.forceVisible(true);
    }),
    dashboardACollaborator.evaluate(() => {
      window.__smoke.forceVisible(true);
    }),
    dashboardBPage.evaluate(() => {
      window.__smoke.forceVisible(true);
    }),
  ]);

  await Promise.all([
    expectDashboardSelection(dashboardAPage, stack.dashboardId),
    expectDashboardSelection(dashboardACollaborator, stack.dashboardId),
    expectDashboardSelection(dashboardBPage, stack.secondDashboardId),
  ]);

  const [workerA, workerB] = await Promise.all([
    stack.startPtyFixtureWorker(),
    stack.startSecondDashboardPtyFixtureWorker(),
  ]);
  await Promise.all([
    waitForScopedWorker(dashboardAPage, workerA.workerFp),
    waitForScopedWorker(dashboardACollaborator, workerA.workerFp),
    waitForScopedWorker(dashboardBPage, workerB.workerFp),
  ]);

  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase();
  const sessionA = await spawnPtyFixtureSession(dashboardAPage, workerA);
  await Promise.all([
    navigateToSmokeSession(dashboardAPage, sessionA),
    navigateToSmokeSession(dashboardACollaborator, sessionA),
  ]);
  await Promise.all([
    waitForPainted(dashboardAPage, sessionA, PTY_FIXTURE_READY),
    waitForPainted(dashboardACollaborator, sessionA, PTY_FIXTURE_READY),
  ]);

  // Positive control: two independently enrolled A browsers still collaborate
  // on their shared terminal before the cross-dashboard attacks begin.
  const sharedMarker = `DASHBOARD-A-SHARED-${suffix}`;
  await inputSmokeTerminal(dashboardAPage, sessionA, encodePtyFixtureCommand({
    op: "EMIT",
    text: sharedMarker,
  }));
  await Promise.all([
    waitForPainted(dashboardAPage, sessionA, sharedMarker),
    waitForPainted(dashboardACollaborator, sessionA, sharedMarker),
  ]);

  const workspaceA = await dashboardAPage.evaluate(async ({ workerFp, folder, sessionId }) => {
    const smoke = window.__smoke;
    return (await smoke.createWorkspace(workerFp, folder, sessionId)).id;
  }, {
    workerFp: workerA.workerFp,
    folder: process.platform === "win32" ? workerA.home.replaceAll("\\", "/") : workerA.home,
    sessionId: sessionA,
  });

  const sessionB = await spawnPtyFixtureSession(dashboardBPage, workerB);
  await navigateToSmokeSession(dashboardBPage, sessionB);
  await waitForPainted(dashboardBPage, sessionB, PTY_FIXTURE_READY);
  const workspaceB = await dashboardBPage.evaluate(async ({ workerFp, folder, sessionId }) => {
    const smoke = window.__smoke;
    return (await smoke.createWorkspace(workerFp, folder, sessionId)).id;
  }, {
    workerFp: workerB.workerFp,
    folder: process.platform === "win32" ? workerB.home.replaceAll("\\", "/") : workerB.home,
    sessionId: sessionB,
  });
  expect(await stack.secondDashboardClient.sessionsAssignWorkspace({
    sessionId: sessionB,
    workspaceId: workspaceB,
  })).toMatchObject({ ok: true });

  const retainedBMarker = `DASHBOARD-B-RETAINED-${suffix}`;
  await inputSmokeTerminal(dashboardBPage, sessionB, encodePtyFixtureCommand({
    op: "EMIT",
    text: retainedBMarker,
  }));
  await waitForPainted(dashboardBPage, sessionB, retainedBMarker);

  const privateFileContents = `DASHBOARD-B-FILE-${suffix}`;
  const privateFilePath = join(workerB.home, `dashboard-b-private-${suffix}.txt`);
  writeFileSync(privateFilePath, privateFileContents);
  const bFile = await dashboardBPage.evaluate(({ workerFp, path }) => {
    const smoke = window.__smoke;
    return smoke.downloadWorkerFile(workerFp, path);
  }, { workerFp: workerB.workerFp, path: privateFilePath });
  expect(bFile.bytes).toBe(privateFileContents.length);

  await Promise.all([
    expectNoDashboardLeak(dashboardAPage, sessionB, workerB.workerFp, retainedBMarker),
    expectNoDashboardLeak(dashboardACollaborator, sessionB, workerB.workerFp, retainedBMarker),
  ]);

  // A guessed B URL must not register an A terminal view or disclose B's
  // retained seed. Return to A before issuing the explicit browser input attack
  // so it travels on an otherwise healthy A terminal Sync domain.
  await dashboardAPage.goto(`${stack.baseUrl}/s/${sessionB}`, { waitUntil: "domcontentloaded" });
  await expect(dashboardAPage.getByTestId(`terminal-slot-${sessionB}`)).toHaveCount(0);
  await expectNoDashboardLeak(dashboardAPage, sessionB, workerB.workerFp, retainedBMarker);
  await navigateToSmokeSession(dashboardAPage, sessionA);
  await waitForPainted(dashboardAPage, sessionA, sharedMarker);

  const liveBMarker = `DASHBOARD-B-LIVE-${suffix}`;
  await inputSmokeTerminal(dashboardBPage, sessionB, encodePtyFixtureCommand({
    op: "EMIT",
    text: liveBMarker,
  }));
  await waitForPainted(dashboardBPage, sessionB, liveBMarker);
  await Promise.all([
    expectNoDashboardLeak(dashboardAPage, sessionB, workerB.workerFp, liveBMarker),
    expectNoDashboardLeak(dashboardACollaborator, sessionB, workerB.workerFp, liveBMarker),
  ]);

  const before = await scopedSession(stack.secondDashboardClient, sessionB);
  const attachmentsBefore = await stack.secondDashboardClient.listAttachments({ sessionId: sessionB });
  const attackMarker = `DASHBOARD-A-INJECT-${suffix}`;
  await expect(dashboardAPage.evaluate(async ({ sessionId, command }) => {
    const smoke = window.__smoke;
    await smoke.input(sessionId, command);
  }, {
    sessionId: sessionB,
    command: encodePtyFixtureCommand({ op: "EMIT", text: attackMarker }),
  })).rejects.toThrow();

  await expectNotFound(() => stack.client.sessionsAttach({ sessionId: sessionB }));
  expect(await stack.client.sessionsRename({ sessionId: sessionB, title: `foreign-title-${suffix}` })).toMatchObject({ ok: false });
  expect(await dashboardAPage.evaluate((sessionId) => {
    const smoke = window.__smoke;
    return smoke.kill(sessionId);
  }, sessionB)).toEqual({ accepted: false });
  expect(await stack.client.sessionsAssignWorkspace({
    sessionId: sessionB,
    workspaceId: workspaceA,
  })).toMatchObject({ ok: false });
  await expectNotFound(() => stack.client.sessionsGetScrollbackCells({
    sessionId: sessionB,
    endRow: 1n,
    maxRows: 1,
    gridEpoch: "",
  }));
  await expect(dashboardAPage.evaluate(({ workerFp, path }) => {
    const smoke = window.__smoke;
    return smoke.downloadWorkerFile(workerFp, path);
  }, { workerFp: workerB.workerFp, path: privateFilePath })).rejects.toThrow();
  await expect(dashboardAPage.evaluate((sessionId) => {
    const smoke = window.__smoke;
    return smoke.attachmentProbe(sessionId, "0".repeat(64), 1, "foreign-attachment.bin");
  }, sessionB)).rejects.toThrow();
  await expectNotFound(() => stack.client.attachFileChunk({
    uploadId: crypto.randomUUID(),
    sessionId: sessionB,
    filename: "foreign-attachment.bin",
    shortPath: false,
    data: new Uint8Array([1]),
    last: true,
    seq: 0,
  }));
  await expectNotFound(() => stack.client.listAttachments({ sessionId: sessionB }));

  const after = await scopedSession(stack.secondDashboardClient, sessionB);
  const attachmentsAfter = await stack.secondDashboardClient.listAttachments({ sessionId: sessionB });
  expect({
    status: after.status,
    workspaceId: after.workspaceId,
    customTitle: after.customTitle,
  }).toEqual({
    status: before.status,
    workspaceId: before.workspaceId,
    customTitle: before.customTitle,
  });
  expect(attachmentsAfter).toEqual(attachmentsBefore);
  const targetBrowser = await dashboardBPage.evaluate((sessionId) => {
    const smoke = window.__smoke;
    return {
      hasSession: !!smoke.state().sessions[sessionId],
      text: smoke.viewportText(sessionId),
    };
  }, sessionB);
  expect(targetBrowser.hasSession).toBe(true);
  expect(targetBrowser.text).not.toContain(attackMarker);
});
