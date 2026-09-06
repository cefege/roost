// Browser proof for metadata navigation search and the unavailable cross-worker transfer surface.
// The multi-worker fixture exposes machine/workspace metadata and the conditional transfer entry.
// Request capture guards the informational transfer dialog from regaining an RPC side effect.

import { test, expect } from "./fixtures.ts";
import { spawnSmokeShell } from "./terminal-helpers.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";

const TRANSFER_RPC_SUFFIXES = [
  "/roost.v1.CoordinatorService/TransfersStart",
  "/roost.v1.CoordinatorService/TransfersOutput",
] as const;

test("metadata search and cross-worker transfer expose their honest surfaces", async ({
  multiWorkerSmokePage,
  stack,
  secondWorker,
}, testInfo) => {
  test.skip(
    !testInfo.project.name.startsWith("chromium"),
    "desktop multi-worker beta-surface contract",
  );

  const customTitle = `metadata-${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}`;
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
