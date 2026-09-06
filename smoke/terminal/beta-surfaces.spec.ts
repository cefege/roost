// Browser proof for dashboard metadata/content search and the unavailable cross-worker transfer surface.
// Two real workers expose distinct retained PTY markers; selecting a global result
// re-runs pane-local find while the transfer item remains informational and side-effect free.

import { test, expect } from "./fixtures.ts";
import { inputSmokeTerminal, spawnSmokeShell } from "./terminal-helpers.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";

const TRANSFER_RPC_SUFFIXES = [
  "/roost.v1.CoordinatorService/TransfersStart",
  "/roost.v1.CoordinatorService/TransfersOutput",
] as const;

test("metadata and global content search preserve the cross-worker transfer beta boundary", async ({
  multiWorkerSmokePage,
  stack,
  secondWorker,
}, testInfo) => {
  test.skip(
    !testInfo.project.name.startsWith("chromium"),
    "desktop multi-worker beta-surface contract",
  );

  const customTitle = `metadata-${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}`;
  const markerStem = `global-${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}`;
  const primaryMarker = `${markerStem}-primary`;
  const secondaryMarker = `${markerStem}-secondary`;
  const paddingTail = `padding-${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}-done`;
  const primarySessionId = (await spawnSmokeShell(
    multiWorkerSmokePage,
    stack.workerFp,
  )).session_id;
  const sessionId = (await spawnSmokeShell(
    multiWorkerSmokePage,
    secondWorker.workerFp,
  )).session_id;
  expect(await stack.client.sessionsRename({ sessionId, title: customTitle })).toMatchObject({
    ok: true,
  });
  await multiWorkerSmokePage.waitForFunction(({ id, title }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.state().sessions[id]?.custom_title === title;
  }, { id: sessionId, title: customTitle });
  await Promise.all([
    inputSmokeTerminal(
      multiWorkerSmokePage,
      primarySessionId,
      `printf '%s\\n' ${primaryMarker}\r`,
    ),
    inputSmokeTerminal(
      multiWorkerSmokePage,
      sessionId,
      `printf '%s\\n' ${secondaryMarker}\r`,
    ),
  ]);
  for (const [id, marker] of [
    [primarySessionId, primaryMarker],
    [sessionId, secondaryMarker],
  ] as const) {
    await multiWorkerSmokePage.evaluate((targetId) => {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      smokeWindow.__smoke.navigate(`/s/${targetId}`);
    }, id);
    const markerSlot = multiWorkerSmokePage.getByTestId(`terminal-slot-${id}`);
    await expect(markerSlot).toBeVisible();
    await expect.poll(() => markerSlot.textContent(), { timeout: 30_000 }).toContain(marker);
  }
  await inputSmokeTerminal(
    multiWorkerSmokePage,
    sessionId,
    `i=0; while [ "$i" -lt 2105 ]; do printf 'padding-%04d\\n' "$i"; i=$((i+1)); done; printf '%s\\n' ${paddingTail}\r`,
  );
  await multiWorkerSmokePage.evaluate((targetId) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.navigate(`/s/${targetId}`);
  }, sessionId);
  await expect.poll(
    () => multiWorkerSmokePage.getByTestId(`terminal-slot-${sessionId}`).textContent(),
    { timeout: 30_000 },
  ).toContain(paddingTail);

  await multiWorkerSmokePage.evaluate((query) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.navigate(`/search?q=${encodeURIComponent(query)}`);
  }, customTitle);
  await expect(multiWorkerSmokePage).toHaveURL(
    `${stack.baseUrl}/search?q=${encodeURIComponent(customTitle)}`,
  );
  await expect(multiWorkerSmokePage.getByTestId("global-search-page")).toBeVisible();
  await expect(
    multiWorkerSmokePage.getByRole("heading", { name: "Search sessions", exact: true }),
  ).toBeVisible();
  const metadataResult = multiWorkerSmokePage.getByTestId(`global-search-result-${sessionId}`);
  await expect(metadataResult).toBeVisible();
  await expect(metadataResult.getByTestId(`global-search-title-${sessionId}`)).toHaveText(
    customTitle,
  );
  await expect(metadataResult).toContainText("/tmp");
  await expect(metadataResult).toContainText("tmp");
  await expect(metadataResult).toContainText(secondWorker.label);
  await expect(metadataResult.getByTestId(`global-search-availability-${sessionId}`))
    .not.toHaveText("Unavailable");

  await metadataResult.click();
  await expect(multiWorkerSmokePage).toHaveURL(`${stack.baseUrl}/s/${sessionId}`);
  await expect(multiWorkerSmokePage.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();

  await multiWorkerSmokePage.evaluate((query) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.navigate(`/search?q=${encodeURIComponent(query)}`);
  }, markerStem);
  const primaryContentResult = multiWorkerSmokePage.locator(
    `[data-testid^="global-content-result-${primarySessionId}-"]`,
  ).first();
  const secondaryContentResult = multiWorkerSmokePage.locator(
    `[data-testid^="global-content-result-${sessionId}-"]`,
  ).first();
  await expect(primaryContentResult).toBeVisible({ timeout: 30_000 });
  await primaryContentResult.click();
  await expect(multiWorkerSmokePage).toHaveURL(`${stack.baseUrl}/s/${primarySessionId}`);
  const primaryTerminal = multiWorkerSmokePage.getByTestId(`terminal-slot-${primarySessionId}`);
  const primaryFindInput = primaryTerminal.getByTestId("terminal-find-input");
  await expect(primaryFindInput).toBeVisible();
  await expect(primaryFindInput).toHaveValue(markerStem);
  await expect(primaryTerminal.getByTestId("terminal-find-count"))
    .toHaveText(/[1-9]\d*\/[1-9]\d*/, { timeout: 30_000 });
  await expect.poll(() => multiWorkerSmokePage.evaluate(({ id, marker }) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    return [...(pane?.querySelectorAll(".cell-find-hit") ?? [])].some((hit) =>
      (hit.closest(".cell-row")?.textContent ?? "").includes(marker)
    );
  }, { id: primarySessionId, marker: primaryMarker }), { timeout: 30_000 }).toBe(true);
  await primaryTerminal.getByTestId("terminal-find-close").click();
  await multiWorkerSmokePage.evaluate((query) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.navigate(`/search?q=${encodeURIComponent(query)}`);
  }, markerStem);
  await expect(primaryContentResult).toBeVisible({ timeout: 30_000 });
  await expect(secondaryContentResult).toHaveCount(0);
  const loadMore = multiWorkerSmokePage.getByTestId("global-content-load-more");
  await expect(loadMore).toBeVisible();
  await loadMore.click();
  await expect(secondaryContentResult).toBeVisible({ timeout: 30_000 });
  await expect(primaryContentResult).toContainText(primaryMarker);
  await expect(secondaryContentResult).toContainText(secondaryMarker);

  await secondaryContentResult.click();
  await expect(multiWorkerSmokePage).toHaveURL(`${stack.baseUrl}/s/${sessionId}`);
  const secondaryTerminal = multiWorkerSmokePage.getByTestId(`terminal-slot-${sessionId}`);
  const secondaryFindInput = secondaryTerminal.getByTestId("terminal-find-input");
  await expect(secondaryFindInput).toBeVisible();
  await expect(secondaryFindInput).toHaveValue(markerStem);
  await expect(secondaryTerminal.getByTestId("terminal-find-count"))
    .toHaveText(/[1-9]\d*\/[1-9]\d*/, { timeout: 30_000 });
  await expect.poll(() => multiWorkerSmokePage.evaluate(({ id, marker }) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    return [...(pane?.querySelectorAll(".cell-find-hit") ?? [])].some((hit) =>
      (hit.closest(".cell-row")?.textContent ?? "").includes(marker)
    );
  }, { id: sessionId, marker: secondaryMarker }), { timeout: 30_000 }).toBe(true);
  await secondaryTerminal.getByTestId("terminal-find-close").click();
  await expect(secondaryFindInput).toHaveCount(0);

  await multiWorkerSmokePage.getByTestId("brand-row-search").click();
  await multiWorkerSmokePage.getByTestId("sidebar-search").fill(customTitle);
  const sessionRow = multiWorkerSmokePage.locator(
    `[data-testid="sidebar-session-row"][data-session-id="${sessionId}"]`,
  );
  await expect(sessionRow).toBeVisible();

  const transferRpcRequests: string[] = [];
  multiWorkerSmokePage.on("request", (request) => {
    const pathname = new URL(request.url()).pathname;
    if (TRANSFER_RPC_SUFFIXES.some((suffix) => pathname.endsWith(suffix))) {
      transferRpcRequests.push(pathname);
    }
  });

  await sessionRow.click({ button: "right" });
  const transferItem = multiWorkerSmokePage.getByTestId(`session-ctx-transfer-${sessionId}`);
  await expect(transferItem).toHaveText("Transfer files (beta)…");
  await transferItem.click();

  await expect(
    multiWorkerSmokePage.getByText("Cross-worker transfer (beta)", { exact: true }),
  ).toBeVisible();
  await expect(multiWorkerSmokePage.getByTestId("transfer-dialog-body")).toHaveText(
    "Cross-worker transfer is not available in v0.5.0. Use the terminal to run rsync or scp.",
  );
  const closeButton = multiWorkerSmokePage.getByTestId("transfer-dialog-close");
  await expect(closeButton).toHaveText("Close");
  await closeButton.click();
  await expect(multiWorkerSmokePage.getByTestId("transfer-dialog-body")).toBeHidden();
  expect(transferRpcRequests).toEqual([]);
});
