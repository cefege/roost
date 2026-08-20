import { mkdirSync } from "node:fs";
import { setTimeout as delay } from "node:timers/promises";
import { fromBinary } from "@bufbuild/protobuf";
import { test, expect } from "./fixtures.ts";
import { join } from "node:path";
import { FilesListDirRequestSchema } from "../../apps/shared/src/gen/roost/v1/coordinator_pb.ts";
import { encodeFolderPath } from "../../apps/web/src/lib/terminalHref.ts";
import type { RecoverySmokeApi, TerminalIdentityProbeWindow } from "./terminal-smoke-api.ts";
import { spawnSmokeShell, navigateToSmokeSession } from "./terminal-helpers.ts";
import {
  installTerminalLoadingStageProbe,
  terminalLoadingStages,
} from "./terminal-loading-stage-probe.ts";

test("browser smoke flow creates and cleans its resources", async ({ smokePage, stack }) => {
  // Pin the shell worker: the shared stack also runs a PTY-FIXTURE worker whose
  // "shell" speaks the fixture protocol, and picking it by recency makes the
  // flow wait out its paint deadline on a session that can never echo.
  const result = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        runFlow(options?: { workerFp?: string }): Promise<{
          steps: Array<{ name: string; pass: boolean; detail: unknown }>;
          summary: string;
        }>;
      };
    }).__smoke;
    return smoke.runFlow({ workerFp });
  }, stack.workerFp);
  expect(result.steps.filter((step) => !step.pass)).toEqual([]);
});

test("cold document shows loading until an existing terminal paints", async ({
  smokePage,
  coldSmokePage,
  stack,
}) => {
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);

  const marker = `COLD-BOOT-${crypto.randomUUID().replaceAll("-", "")}`;
  await smokePage.evaluate(async ({ id, command }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, command);
  }, { id: sessionId, command: `printf '${marker}\\n'\r` });
  const seededProof = await smokePage.evaluate(({ id, expected }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, expected, 30_000);
  }, { id: sessionId, expected: marker });
  expect(seededProof).toMatchObject({
    proof_kind: "marker",
    sessionId,
    marker,
    frames: 2,
  });

  await installTerminalLoadingStageProbe(coldSmokePage, true);

  const sessionsListPattern = "**/roost.v1.CoordinatorService/SessionsList";
  let releaseSessionsList!: () => void;
  let finishHeldRoute!: () => void;
  const sessionsListGate = new Promise<void>((resolve) => {
    releaseSessionsList = resolve;
  });
  const heldRouteFinished = new Promise<void>((resolve) => {
    finishHeldRoute = resolve;
  });
  let routeIntercepted = false;
  await coldSmokePage.route(sessionsListPattern, async (route) => {
    routeIntercepted = true;
    try {
      await sessionsListGate;
      await route.continue();
    } finally {
      finishHeldRoute();
    }
  });

  const targetUrl = `${stack.baseUrl}/s/${sessionId}`;
  const documentRequests: string[] = [];
  coldSmokePage.on("request", (request) => {
    if (request.isNavigationRequest() && request.frame() === coldSmokePage.mainFrame()) {
      documentRequests.push(request.url());
    }
  });

  try {
    const sessionsListRequest = coldSmokePage.waitForRequest(sessionsListPattern);
    await coldSmokePage.goto(targetUrl, { waitUntil: "commit" });
    await sessionsListRequest;
    await expect.poll(() => routeIntercepted).toBe(true);

    const loadingStatus = coldSmokePage.getByTestId("terminal-loading-status");
    await expect(loadingStatus).toHaveAttribute("data-stage", "sessions");
    await expect(coldSmokePage.getByTestId("terminal-loading-title"))
      .toHaveText("Loading terminal sessions");
    await expect(coldSmokePage.getByTestId("terminal-loading-detail"))
      .toHaveText("Live terminal channel opened; waiting for the session list.");
    await expect.poll(() => terminalLoadingStages(coldSmokePage)).toContain("sessions");
    await expect.poll(async () => {
      return loadingStatus.evaluate((status) => {
        const elapsedSeconds = Number(status.getAttribute("data-elapsed-seconds"));
        const elapsedCopy = status.querySelector(
          '[data-testid="terminal-loading-elapsed"]',
        )?.textContent;
        return elapsedSeconds >= 1 &&
          elapsedCopy === `This step has taken ${elapsedSeconds}s`;
      });
    }).toBe(true);
    releaseSessionsList();
    await heldRouteFinished;
    await expect(coldSmokePage.getByTestId(`terminal-slot-${sessionId}`))
      .toBeVisible({ timeout: 30_000 });
    const coldBootProof = await coldSmokePage.evaluate(({ id, expected }) => {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      return smokeWindow.__smoke.waitForPaintedMarker(id, expected, 30_000);
    }, { id: sessionId, expected: marker });
    expect(coldBootProof).toMatchObject({
      proof_kind: "marker",
      sessionId,
      marker,
      frames: 2,
    });
    await expect(coldSmokePage.getByTestId("terminal-loading-status")).toHaveCount(0);
    await expect(coldSmokePage).toHaveURL(targetUrl);
    expect(documentRequests).toEqual([targetUrl]);
  } finally {
    releaseSessionsList();
    if (routeIntercepted) await heldRouteFinished.catch(() => undefined);
    await coldSmokePage.unroute(sessionsListPattern);
  }
});

test("new-terminal server switch resets browse path before listing and spawning", async ({
  multiWorkerSmokePage,
  stack,
  secondWorker,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop multi-worker browse contract");

  const suffix = crypto.randomUUID().slice(0, 8);
  const aChildName = `a-only-${suffix}`;
  const aChildPath = join(stack.workerHome, aChildName);
  const bDefaultPath = join(secondWorker.home, `b-default-${suffix}`);
  mkdirSync(aChildPath, { recursive: true });
  mkdirSync(bDefaultPath, { recursive: true });

  const bSeedSessionId = await multiWorkerSmokePage.evaluate(async ({ workerFp, cwd }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return (await smokeWindow.__smoke.spawnShell(workerFp, cwd)).session_id;
  }, { workerFp: secondWorker.workerFp, cwd: bDefaultPath });
  await multiWorkerSmokePage.waitForFunction((sessionId) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return !!smokeWindow.__smoke.state().sessions[sessionId]?.cwd;
  }, bSeedSessionId);

  // FlatNewTerminal chooses the globally newest session. Cross a timestamp
  // boundary, then seed A so the sidebar-scoped plus deterministically opens A.
  await delay(10);
  const aSeedSessionId = await multiWorkerSmokePage.evaluate(async ({ workerFp, cwd }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return (await smokeWindow.__smoke.spawnShell(workerFp, cwd)).session_id;
  }, { workerFp: stack.workerFp, cwd: stack.workerHome });
  await multiWorkerSmokePage.waitForFunction((sessionId) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return !!smokeWindow.__smoke.state().sessions[sessionId]?.cwd;
  }, aSeedSessionId);

  const seedCwds = await multiWorkerSmokePage.evaluate(({ aId, bId }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const sessions = smokeWindow.__smoke.state().sessions;
    return { a: sessions[aId]?.cwd, b: sessions[bId]?.cwd };
  }, { aId: aSeedSessionId, bId: bSeedSessionId });
  expect(seedCwds).toEqual({ a: stack.workerHome, b: bDefaultPath });

  const listRequests: Array<{ workerFp: string; path: string }> = [];
  const decodeErrors: string[] = [];
  multiWorkerSmokePage.on("request", (request) => {
    if (!new URL(request.url()).pathname.endsWith("/roost.v1.CoordinatorService/FilesListDir")) return;
    const body = request.postDataBuffer();
    if (!body) {
      decodeErrors.push("FilesListDir request had no body");
      return;
    }
    try {
      const decoded = fromBinary(FilesListDirRequestSchema, body);
      listRequests.push({ workerFp: decoded.workerFp, path: decoded.path });
    } catch (error) {
      decodeErrors.push(String(error));
    }
  });

  await multiWorkerSmokePage
    .getByTestId("folder-list")
    .getByTestId("flat-new-terminal-button")
    .click();
  await expect(multiWorkerSmokePage).toHaveURL(`${stack.baseUrl}/browse/${stack.workerFp}`);
  await expect(multiWorkerSmokePage.getByTestId("browse-server")).toHaveAttribute("title", "roost-terminal-test");
  await expect(multiWorkerSmokePage.getByTestId("browse-crumb").last()).toHaveAttribute("title", stack.workerHome);

  const aFolder = multiWorkerSmokePage
    .locator('[data-testid="browse-tile"], [data-testid="browse-row"]')
    .filter({ hasText: aChildName });
  await expect(aFolder).toHaveCount(1);
  await aFolder.click();
  await expect(multiWorkerSmokePage.getByTestId("browse-crumb").last()).toHaveAttribute("title", aChildPath);
  await expect(multiWorkerSmokePage.getByTestId("browse-back")).toBeEnabled();

  await multiWorkerSmokePage.getByTestId("browse-server").click();
  await multiWorkerSmokePage
    .getByTestId("browse-server-option")
    .filter({ hasText: secondWorker.label })
    .click();
  await expect(multiWorkerSmokePage).toHaveURL(`${stack.baseUrl}/browse/${secondWorker.workerFp}`);
  await expect(multiWorkerSmokePage.getByTestId("browse-server")).toHaveAttribute("title", secondWorker.label);
  await expect(multiWorkerSmokePage.getByTestId("browse-crumb").last()).toHaveAttribute("title", bDefaultPath);
  await expect(multiWorkerSmokePage.getByTestId("browse-back")).toBeDisabled();

  await expect.poll(
    () => listRequests.some((request) =>
      request.workerFp === secondWorker.workerFp && request.path === bDefaultPath),
  ).toBe(true);
  expect(decodeErrors).toEqual([]);
  expect(listRequests).not.toContainEqual({ workerFp: secondWorker.workerFp, path: aChildPath });

  await multiWorkerSmokePage.getByTestId("browse-open").click();
  await expect(multiWorkerSmokePage).toHaveURL(
    `${stack.baseUrl}/t/${secondWorker.workerFp}/${encodeFolderPath(bDefaultPath)}`,
  );
  await expect.poll(() => multiWorkerSmokePage.evaluate(({ workerFp, cwd, seedId }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const sessions = smokeWindow.__smoke.state().sessions;
    return Object.values(sessions).filter((session) =>
      session.id !== seedId
      && session.worker_fp === workerFp
      && session.cwd === cwd
      && session.spawn_cwd === cwd
    ).length;
  }, { workerFp: secondWorker.workerFp, cwd: bDefaultPath, seedId: bSeedSessionId })).toBe(1);
});

test("same session metadata updates preserve the mounted terminal DOM", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop terminal identity contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as TerminalIdentityProbeWindow;
    return smokeWindow.__smoke.cellFullFrameCount(id);
  }, sessionId)).toBeGreaterThan(0);

  await smokePage.evaluate((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    const grid = slot?.querySelector(".cell-grid");
    const textarea = slot?.querySelector("textarea");
    if (!slot || !grid || !textarea) throw new Error("terminal DOM identity probe could not be installed");
    const smokeWindow = window as unknown as TerminalIdentityProbeWindow;
    smokeWindow.__terminalIdentityProbe = { slot, grid, textarea };
  }, sessionId);

  // Folder rows collapse sessions by cwd; search switches the sidebar to the
  // per-session rows that carry the rename menu (same surface agent-status uses).
  await smokePage.getByTestId("brand-row-search").click();
  await smokePage.getByTestId("sidebar-search").fill("/tmp");
  const sessionRow = smokePage.locator(
    `[data-testid="sidebar-session-row"][data-session-id="${sessionId}"]`,
  );
  await sessionRow.click({ button: "right" });
  await smokePage.getByTestId(`session-ctx-rename-${sessionId}`).click();
  const renameInput = smokePage.getByTestId("rename-input");
  const customTitle = `identity-${sessionId.slice(0, 8)}`;
  await renameInput.evaluate((element, title) => {
    const field = element as HTMLElement & { value: string };
    field.value = title;
    field.dispatchEvent(new Event("input", { bubbles: true, composed: true }));
  }, customTitle);
  await smokePage.getByTestId("rename-confirm").click();
  await expect(sessionRow).toContainText(customTitle);

  expect(await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as TerminalIdentityProbeWindow;
    const prior = smokeWindow.__terminalIdentityProbe;
    const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    return {
      slot: slot === prior.slot,
      grid: slot?.querySelector(".cell-grid") === prior.grid,
      textarea: slot?.querySelector("textarea") === prior.textarea,
      connected: prior.slot.isConnected && prior.grid.isConnected && prior.textarea.isConnected,
    };
  }, sessionId)).toEqual({
    slot: true,
    grid: true,
    textarea: true,
    connected: true,
  });

  const marker = `IDENTITY-SURVIVED-${sessionId}`;
  await smokePage.evaluate(async ({ id, command }) => {
    const smokeWindow = window as unknown as TerminalIdentityProbeWindow;
    await smokeWindow.__smoke.input(id, command);
  }, { id: sessionId, command: `printf '${marker}\\n'\r` });
  await expect.poll(() => smokePage.getByTestId(`terminal-slot-${sessionId}`).textContent())
    .toContain(marker);
});
