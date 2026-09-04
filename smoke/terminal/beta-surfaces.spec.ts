// Browser proof for the discoverable-but-unavailable search and cross-worker transfer surfaces.
// The existing multi-worker fixture makes the conditional transfer menu entry observable.
// Request capture guards the informational dialog from regaining a transfer RPC side effect.

import { test, expect } from "./fixtures.ts";
import { spawnSmokeShell } from "./terminal-helpers.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";

const TRANSFER_RPC_SUFFIXES = [
  "/roost.v1.CoordinatorService/TransfersStart",
  "/roost.v1.CoordinatorService/TransfersOutput",
] as const;

test("search and cross-worker transfer are honest beta surfaces", async ({
  multiWorkerSmokePage,
  stack,
}, testInfo) => {
  test.skip(
    !testInfo.project.name.startsWith("chromium"),
    "desktop multi-worker beta-surface contract",
  );

  const sessionId = (await spawnSmokeShell(multiWorkerSmokePage, stack.workerFp)).session_id;
  await multiWorkerSmokePage.waitForFunction((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return !!smokeWindow.__smoke.state().sessions[id];
  }, sessionId);

  await multiWorkerSmokePage.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.navigate("/search");
  });
  await expect(multiWorkerSmokePage).toHaveURL(`${stack.baseUrl}/search`);
  await expect(
    multiWorkerSmokePage.getByText("Global search (beta)", { exact: true }),
  ).toBeVisible();
  await expect(
    multiWorkerSmokePage.getByText(
      "Global search is not available in v0.5.0. Use sidebar filtering or terminal find.",
      { exact: true },
    ),
  ).toBeVisible();

  await multiWorkerSmokePage.getByTestId("brand-row-search").click();
  await multiWorkerSmokePage.getByTestId("sidebar-search").fill("/tmp");
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
