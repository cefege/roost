#!/usr/bin/env bun
// Finite release proof for an exact compiled worker artifact.
// It runs the artifact through the real terminal stack, production SPA, browser UI, and CLI.
// The stack owns isolated HOME/data and verifies the binary self-execs a keeper across restart.

import { accessSync, constants, existsSync, realpathSync, statSync } from "node:fs";
import { strict as assert } from "node:assert";
import { chromium, expect, type Browser } from "@playwright/test";
import { join } from "node:path";
import { startTerminalTestStack, type TerminalTestStack } from "../terminal/stack.ts";
import { waitForKeeperChannels, waitForKeeperRowAfter } from "../terminal/terminal-local-fast-path-helpers.ts";
import { REPOSITORY_ROOT } from "../terminal/stack-runtime.ts";
import {
  enterPackagedWorkerCommand,
  expectPackagedWorkerTransport,
  openPackagedWorkerPage,
  openPackagedWorkerSession,
  type PackagedWorkerPage,
} from "./qualify-packaged-worker-browser.ts";
import { verifyPackagedCliInput } from "./qualify-packaged-worker-cli.ts";

const RESTART_TRANSPORT_TIMEOUT_MS = 120_000;
const TERMINAL_OUTPUT_TIMEOUT_MS = 60_000;

type PackagedWorkerOptions = {
  readonly binary: string;
};

async function main(): Promise<void> {
  const options = parseOptions(process.argv.slice(2));
  requireProductionSpa();
  if (process.platform === "win32") {
    throw new Error("packaged worker qualification is POSIX-only while Windows worker releases are paused");
  }

  let stack: TerminalTestStack | undefined;
  let browser: Browser | undefined;
  let direct: PackagedWorkerPage | undefined;
  let fallback: PackagedWorkerPage | undefined;
  try {
    stack = await startTerminalTestStack({
      workerExecutable: options.binary,
      useRealHome: false,
      terminalPeer: {
        coordinatorEnabled: true,
        coordinatorStunUrls: [],
        workerEnabled: true,
        workerPortRange: { min: 40_000, max: 40_031 },
        disableLoopbackProbe: true,
      },
    });
    assert.notEqual(stack.workerHome, process.env.HOME, "packaged worker must not use the runner HOME");

    browser = await chromium.launch();
    direct = await openPackagedWorkerPage(browser, stack, { disableWebRtc: false });
    const sessionId = (await stack.client.sessionsSpawn({
      workerFp: stack.workerFp,
      kind: "shell",
      folder: stack.workerHome,
    })).sessionId;
    await openPackagedWorkerSession(direct.page, stack, sessionId);
    await expectPackagedWorkerTransport(direct.page, sessionId, "webrtc");

    const directMarker = marker("PACKAGED_DIRECT");
    await enterPackagedWorkerCommand(
      direct.page,
      sessionId,
      `printf '%s\\n' ${directMarker}`,
      directMarker,
    );

    const cliMarker = marker("PACKAGED_CLI");
    await verifyPackagedCliInput(options.binary, stack, sessionId, cliMarker);
    await expect(direct.page.getByTestId(`terminal-slot-${sessionId}`))
      .toContainText(cliMarker, { timeout: TERMINAL_OUTPUT_TIMEOUT_MS });

    fallback = await openPackagedWorkerPage(browser, stack, { disableWebRtc: true });
    await openPackagedWorkerSession(fallback.page, stack, sessionId);
    await expectPackagedWorkerTransport(fallback.page, sessionId, "sync");
    const fallbackMarker = marker("PACKAGED_FALLBACK");
    await enterPackagedWorkerCommand(
      fallback.page,
      sessionId,
      `printf '%s\\n' ${fallbackMarker}`,
      fallbackMarker,
    );

    const keeperBefore = await waitForKeeperChannels(stack, stack.workerFp, 1);
    const workerProcessIdBefore = stack.workerPid();
    assert.ok(workerProcessIdBefore !== undefined, "packaged worker process ID was unavailable before restart");
    await stack.restartWorker();
    const keeperAfter = await waitForKeeperRowAfter(stack, stack.workerFp, keeperBefore.lastSeenMs);
    const workerProcessIdAfter = stack.workerPid();
    assert.ok(workerProcessIdAfter !== undefined, "packaged worker process ID was unavailable after restart");
    assert.notEqual(workerProcessIdAfter, workerProcessIdBefore, "packaged worker restart did not replace its process");
    assertKeeperSurvivedRestart(keeperBefore.runtime, keeperAfter.runtime);

    await expectPackagedWorkerTransport(
      direct.page,
      sessionId,
      "webrtc",
      RESTART_TRANSPORT_TIMEOUT_MS,
    );
    const restartMarker = marker("PACKAGED_RESTART");
    await enterPackagedWorkerCommand(
      direct.page,
      sessionId,
      `printf '%s\\n' ${restartMarker}`,
      restartMarker,
    );
    process.stdout.write("packaged worker qualification passed\n");
  } finally {
    await fallback?.close().catch(() => undefined);
    await direct?.close().catch(() => undefined);
    await browser?.close().catch(() => undefined);
    await stack?.stop();
  }
}

function parseOptions(argumentsList: readonly string[]): PackagedWorkerOptions {
  if (argumentsList.length !== 2 || argumentsList[0] !== "--binary") {
    throw new Error("usage: bun smoke/peer/qualify-packaged-worker.ts --binary <artifact>");
  }
  const suppliedPath = argumentsList[1];
  if (!suppliedPath || suppliedPath.startsWith("--")) {
    throw new Error("--binary requires one artifact path");
  }
  const binary = realpathSync(suppliedPath);
  if (!statSync(binary).isFile()) throw new Error(`packaged worker artifact is not a regular file: ${binary}`);
  accessSync(binary, constants.X_OK);
  return { binary };
}

function requireProductionSpa(): void {
  const index = join(REPOSITORY_ROOT, "apps", "web", "dist", "index.html");
  if (!existsSync(index)) {
    throw new Error("production SPA is missing; build apps/web before packaged worker qualification");
  }
}

function marker(prefix: string): string {
  return `${prefix}-${crypto.randomUUID().replaceAll("-", "").slice(0, 12)}`;
}

function assertKeeperSurvivedRestart(
  before: { keeper_pid: number; keeper_epoch: string; channel_count: number; binding_digest: string },
  after: { keeper_pid: number; keeper_epoch: string; channel_count: number; binding_digest: string },
): void {
  assert.equal(after.keeper_pid, before.keeper_pid, "packaged worker restart replaced its keeper process");
  assert.equal(after.keeper_epoch, before.keeper_epoch, "packaged worker restart changed its keeper epoch");
  assert.equal(after.channel_count, before.channel_count, "packaged worker restart changed live keeper channels");
  assert.equal(after.binding_digest, before.binding_digest, "packaged worker restart changed keeper bindings");
}

try {
  await main();
} catch (error) {
  process.stderr.write(`packaged worker qualification failed: ${String(error)}\n`);
  process.exitCode = 1;
}
