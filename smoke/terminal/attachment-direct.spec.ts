// Real-stack attachment delivery proof for every browser upload carrier.
// It drives the visible composer through a real coordinator, worker, keeper, and browser,
// verifies destination bytes, and treats coordinator AttachFileChunk requests as relay evidence.
// The loopback case also pins the browser-local preview while its dedup probe is held.

import { readFileSync } from "node:fs";
import type { Browser, Page, TestInfo } from "@playwright/test";
import { expect, test } from "./fixtures.ts";
import { startTerminalTestStack, type TerminalTestStack } from "./stack.ts";
import { openPeerSmokePage } from "./terminal-peer-helpers.ts";
import { attachStackLogs, type EnrolledPage } from "./terminal-local-fast-path-helpers.ts";
import {
  navigateToSmokeSession,
  readWorkerBytes,
  spawnSmokeShell,
} from "./terminal-helpers.ts";

const PEER_STACK = {
  terminalPeer: {
    coordinatorEnabled: true,
    coordinatorStunUrls: [],
    workerEnabled: true,
    disableLoopbackProbe: true,
  },
} as const;

type AttachmentRequests = { grant: number; relay: number };

function observeAttachmentRequests(page: Page): AttachmentRequests {
  const counts = { grant: 0, relay: 0 };
  page.on("request", (request) => {
    const url = request.url();
    if (url.includes("/AttachmentsGrantDirect")) counts.grant += 1;
    if (url.includes("/AttachFileChunk")) counts.relay += 1;
  });
  return counts;
}

async function chooseAttachment(
  page: Page,
  sessionId: string,
  file: { name: string; mimeType: string; buffer: Buffer },
): Promise<void> {
  const slot = page.getByTestId(`terminal-slot-${sessionId}`);
  const attach = slot.getByTestId("chat-attach");
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    attach.click(),
  ]);
  await chooser.setFiles(file);
}

async function expectStoredBytes(
  stack: TerminalTestStack,
  sessionId: string,
  filename: string,
  expected: Buffer,
): Promise<void> {
  await expect.poll(async () => {
    const response = await stack.client.listAttachments({ sessionId });
    return response.entries.some((entry) => entry.filename === filename);
  }, { timeout: 30_000 }).toBe(true);
  const entry = (await stack.client.listAttachments({ sessionId })).entries.find(
    (candidate) => candidate.filename === filename,
  );
  if (!entry) throw new Error("uploaded attachment was not listed");
  expect(await readWorkerBytes(stack.client, stack.workerFp, entry.absPath))
    .toEqual(new Uint8Array(expected));
}

async function stopStack(
  stack: TerminalTestStack,
  page: EnrolledPage | undefined,
  testInfo: TestInfo,
): Promise<void> {
  try {
    if (testInfo.status !== testInfo.expectedStatus) await attachStackLogs(testInfo, stack);
  } finally {
    try { await page?.close(); } finally { await stack.stop(); }
  }
}

test("matching loopback uploads bytes directly and previews the selected image locally", async ({ browser }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "direct attachment browser contract");
  test.setTimeout(180_000);
  const stack = await startTerminalTestStack();
  let enrolled: EnrolledPage | undefined;
  try {
    enrolled = await openPeerSmokePage(browser, stack, {
      localWorkerOrigin: stack.localUiUrl(stack.workerFp),
    });
    const page = enrolled.page;
    const sessionId = (await spawnSmokeShell(page, stack.workerFp)).session_id;
    await navigateToSmokeSession(page, sessionId);
    const requests = observeAttachmentRequests(page);
    const probeSeen = Promise.withResolvers<void>();
    const releaseProbe = Promise.withResolvers<void>();
    await page.route("**/roost.v1.CoordinatorService/AttachmentProbe", async (route) => {
      probeSeen.resolve();
      await releaseProbe.promise;
      await route.continue();
    });
    const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8);
    const filename = `direct-loopback-${suffix}.png`;
    const bytes = readFileSync(new URL("../../apps/web/public/icon-32.png", import.meta.url));
    await chooseAttachment(page, sessionId, { name: filename, mimeType: "image/png", buffer: bytes });
    await probeSeen.promise;
    const preview = page.getByTestId("transfer-preview");
    await expect(preview).toBeVisible();
    await expect(preview).toHaveAttribute("src", /^blob:/);
    await expect(preview).toHaveAttribute("alt", "");
    releaseProbe.resolve();
    await expectStoredBytes(stack, sessionId, filename, bytes);
    expect(requests.grant).toBeGreaterThan(0);
    expect(requests.relay).toBe(0);
    await expect.poll(() => readFileSync(stack.workerLogPath, "utf8"), { timeout: 10_000 })
      .toContain('"upload_completed","carrier":"loopback"');
  } finally {
    await stopStack(stack, enrolled, testInfo);
  }
});

test("remote WebRTC attachment peer bypasses coordinator byte relay", async ({ browser }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "direct attachment browser contract");
  test.setTimeout(240_000);
  const stack = await startTerminalTestStack(PEER_STACK);
  let enrolled: EnrolledPage | undefined;
  try {
    enrolled = await openPeerSmokePage(browser, stack);
    const page = enrolled.page;
    const sessionId = (await spawnSmokeShell(page, stack.workerFp)).session_id;
    await navigateToSmokeSession(page, sessionId);
    const requests = observeAttachmentRequests(page);
    const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8);
    const filename = `direct-webrtc-${suffix}.bin`;
    const bytes = Buffer.alloc(1024 * 1024 + 37);
    for (let index = 0; index < bytes.length; index += 1) bytes[index] = (index * 31 + 7) & 0xff;
    await chooseAttachment(page, sessionId, { name: filename, mimeType: "application/octet-stream", buffer: bytes });
    await expectStoredBytes(stack, sessionId, filename, bytes);
    expect(requests.grant).toBeGreaterThan(0);
    expect(requests.relay).toBe(0);
    await expect.poll(() => readFileSync(stack.workerLogPath, "utf8"), { timeout: 10_000 })
      .toContain('"upload_completed","carrier":"webrtc"');
  } finally {
    await stopStack(stack, enrolled, testInfo);
  }
});

test("unavailable direct carriers fall back before sequence zero", async ({ browser }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "attachment fallback browser contract");
  test.setTimeout(180_000);
  const stack = await startTerminalTestStack({
    terminalPeer: { coordinatorEnabled: false, workerEnabled: false, disableLoopbackProbe: true },
  });
  let enrolled: EnrolledPage | undefined;
  try {
    enrolled = await openPeerSmokePage(browser, stack, { rtcUnavailable: true });
    const page = enrolled.page;
    const sessionId = (await spawnSmokeShell(page, stack.workerFp)).session_id;
    await navigateToSmokeSession(page, sessionId);
    const requests = observeAttachmentRequests(page);
    const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8);
    const filename = `relay-fallback-${suffix}.bin`;
    const bytes = Buffer.alloc(700_000, 0x5a);
    await chooseAttachment(page, sessionId, { name: filename, mimeType: "application/octet-stream", buffer: bytes });
    await expectStoredBytes(stack, sessionId, filename, bytes);
    expect(requests.grant).toBe(0);
    expect(requests.relay).toBeGreaterThan(0);
  } finally {
    await stopStack(stack, enrolled, testInfo);
  }
});
