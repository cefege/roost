// Observables the upgrade specs assert on: PTYs opened and marked through a
// real browser, the coordinator's keeper runtime row, the classification the
// product's own admission assigns to this release, and the deploy invocation.
// Every keeper judgement here comes from apps/roost-cli and apps/shared code,
// so the gate cannot drift from what a real deploy would decide.

import { spawn } from "node:child_process";
import { join } from "node:path";
import { expect, type Browser, type Page, type TestInfo } from "@playwright/test";
import {
  loadSourceKeeperContract,
  targetKeeperContractForWorker,
} from "../../apps/roost-cli/src/push-keeper-admission.ts";
import { workerInventoryForUpdateAdmission } from "../../apps/roost-cli/src/status-report.ts";
import type { WorkerStatus } from "../../apps/roost-cli/src/status-types.ts";
import {
  classifyKeeperUpdate,
  type KeeperRuntimeObservationV1,
  type KeeperUpdateClassification,
} from "../../apps/shared/src/keeper-update.ts";
import { supportedHostPlatform } from "../../apps/shared/src/platform.ts";
import { enrollDashboardBrowser } from "../terminal/fixtures.ts";
import { REPOSITORY_ROOT, waitFor } from "../terminal/stack-runtime.ts";
import type { TerminalReleaseCheckout, TerminalTestStack } from "../terminal/stack.ts";
import { spawnSmokeShell, navigateToSmokeSession } from "../terminal/terminal-helpers.ts";
import type { RecoverySmokeApi } from "../terminal/terminal-smoke-api.ts";
import type { UpgradeInstall } from "./fixtures.ts";

const TERMINAL_COUNT = 2;
const MARKER_PAINT_TIMEOUT_MS = 30_000;
// A worker beats every 30s, so a keeper change can take that long to reach the
// coordinator row every keeper assertion reads.
const KEEPER_OBSERVATION_TIMEOUT_MS = 60_000;
const DEPLOY_TIMEOUT_MS = 180_000;

export interface MarkedTerminal {
  sessionId: string;
  marker: string;
}

export interface ReleaseHandoffResult {
  exitCode: number;
  stdout: string;
  stderr: string;
  workerPidFilePath: string;
}

export function workerRow(stack: TerminalTestStack): WorkerStatus {
  const matches = workerInventoryForUpdateAdmission(stack.coordDbPath)
    .filter((worker) => worker.fingerprint === stack.workerFp);
  if (matches.length !== 1) {
    throw new Error(`coordinator database holds ${matches.length} rows for the primary worker`);
  }
  return matches[0]!;
}

export function keeperRuntimeOrThrow(worker: WorkerStatus): KeeperRuntimeObservationV1 {
  if (!worker.keeperRuntime) {
    throw new Error(`${worker.label}: coordinator holds no keeper runtime observation`);
  }
  return worker.keeperRuntime;
}

/** Wait until the coordinator's row proves the keeper holds `channels`. */
export function waitForKeeperChannels(
  stack: TerminalTestStack,
  channels: number,
): Promise<KeeperRuntimeObservationV1> {
  return waitFor(`keeper reporting ${channels} channel(s)`, KEEPER_OBSERVATION_TIMEOUT_MS, () => {
    const observation = workerRow(stack).keeperRuntime;
    return observation?.channel_count === channels ? observation : undefined;
  });
}

/** Wait for a keeper reconciliation newer than `sinceReconciledAtMs`, so a
 *  keeper assertion reads what the upgraded worker found rather than the row
 *  the pre-upgrade worker left behind. */
export function waitForKeeperReconciliationAfter(
  stack: TerminalTestStack,
  sinceReconciledAtMs: number,
): Promise<KeeperRuntimeObservationV1> {
  return waitFor("keeper reconciliation after the deploy", KEEPER_OBSERVATION_TIMEOUT_MS, () => {
    const observation = workerRow(stack).keeperRuntime;
    return observation && observation.reconciled_at_ms > sinceReconciledAtMs
      ? observation
      : undefined;
  });
}

/** Wait for the next heartbeat from the worker that is still running, so a
 *  "nothing was destroyed" assertion reads state observed after the attempt. */
export function waitForHeartbeatAfter(
  stack: TerminalTestStack,
  sinceLastSeenMs: number,
): Promise<KeeperRuntimeObservationV1> {
  return waitFor("worker heartbeat after the refusal", KEEPER_OBSERVATION_TIMEOUT_MS, () => {
    const worker = workerRow(stack);
    return worker.lastSeenMs > sinceLastSeenMs && worker.keeperRuntime
      ? worker.keeperRuntime
      : undefined;
  });
}

/** Open two PTYs in a real browser, write a distinct marker into each, prove
 *  both painted, then close the browser: the markers must survive the upgrade
 *  in the keeper, not in a page that happened to stay open. */
export async function openMarkedTerminals(
  browser: Browser,
  stack: TerminalTestStack,
  testInfo: TestInfo,
): Promise<MarkedTerminal[]> {
  const terminals: MarkedTerminal[] = [];
  await withUpgradePage(browser, stack, testInfo, async (page) => {
    for (let index = 0; index < TERMINAL_COUNT; index += 1) {
      const sessionId = (await spawnSmokeShell(page, stack.workerFp)).session_id;
      const marker = `UPGRADE-${index}-${crypto.randomUUID().replaceAll("-", "")}`;
      const terminal = { sessionId, marker };
      await navigateToSmokeSession(page, sessionId);
      await page.evaluate(async ({ id, command }) => {
        const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
        await smokeWindow.__smoke.input(id, command);
      }, { id: sessionId, command: `printf '${marker}\\n'\r` });
      // Prove it painted now, while this pane is the selected one: only the
      // selected session is in layout, so a marker cannot be proven for a
      // background pane.
      await provePaintedMarker(page, terminal);
      terminals.push(terminal);
    }
  });
  await waitForKeeperChannels(stack, TERMINAL_COUNT);
  return terminals;
}

/** Prove every marker still paints, from a browser that boots after the
 *  upgrade: the bytes come back from the surviving keeper's scrollback. */
export async function waitForPaintedMarkers(
  browser: Browser,
  stack: TerminalTestStack,
  testInfo: TestInfo,
  terminals: readonly MarkedTerminal[],
): Promise<void> {
  await withUpgradePage(browser, stack, testInfo, async (page) => {
    for (const terminal of terminals) {
      await navigateToSmokeSession(page, terminal.sessionId);
      await provePaintedMarker(page, terminal);
    }
  });
}

/** The action the product's own admission requires of this release, computed
 *  before the deploy runs so the deploy is held to it. */
export async function targetKeeperClassification(
  stack: TerminalTestStack,
  gitSha: string,
): Promise<KeeperUpdateClassification> {
  const worker = workerRow(stack);
  const targetContract = targetKeeperContractForWorker(
    await loadSourceKeeperContract(REPOSITORY_ROOT),
    gitSha,
    { bun_abi: Bun.version, platform: supportedHostPlatform(), arch: process.arch },
  );
  return classifyKeeperUpdate(
    targetContract,
    worker.keeperRuntime,
    new Set(worker.coordinatorOpenSessionIds),
  );
}

function releaseHandoffCommand(
  install: UpgradeInstall,
  installedWorkerPid: number,
  release: TerminalReleaseCheckout,
): string[] {
  return [
    process.execPath,
    join(REPOSITORY_ROOT, "smoke", "upgrade", "release-handoff.ts"),
    `--worker-service-spec=${install.stack.workerServiceSpecPath}`,
    `--coord-db=${install.stack.coordDbPath}`,
    `--coordinator-url=${install.stack.baseUrl}`,
    `--api-key=${install.stack.apiKeyPath}`,
    `--dashboard-id=${install.stack.dashboardId}`,
    `--host=${workerRow(install.stack).label}`,
    `--source-root=${release.sourceRoot}`,
    `--git-sha=${release.gitSha}`,
    `--installed-worker-pid=${installedWorkerPid}`,
    `--worker-pid-file=${deployedWorkerPidFilePath(install)}`,
  ];
}

/** Run the deploy the way an operator does — one process, one exit code — and
 *  hand the worker it leaves running to the stack's teardown. */
export async function runReleaseHandoff(
  install: UpgradeInstall,
  installedWorkerPid: number,
  release: TerminalReleaseCheckout = {
    sourceRoot: REPOSITORY_ROOT,
    gitSha: install.workingTreeGitSha,
  },
): Promise<ReleaseHandoffResult> {
  const command = releaseHandoffCommand(install, installedWorkerPid, release);
  const child = spawn(command[0]!, command.slice(1), {
    cwd: REPOSITORY_ROOT,
    stdio: ["ignore", "pipe", "pipe"],
    timeout: DEPLOY_TIMEOUT_MS,
  });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk: string) => { stdout += chunk; });
  child.stderr.on("data", (chunk: string) => { stderr += chunk; });
  const settled = Promise.withResolvers<number>();
  child.on("error", (error) => settled.reject(error));
  child.on("exit", (code) => settled.resolve(code ?? 1));
  const exitCode = await settled.promise;
  const workerPidFilePath = deployedWorkerPidFilePath(install);
  const deployed = Number(await Bun.file(workerPidFilePath).text().catch(() => ""));
  if (Number.isSafeInteger(deployed) && deployed > 0) {
    install.stack.adoptDeployedWorker(deployed);
  }
  return { exitCode, stdout, stderr, workerPidFilePath };
}

function deployedWorkerPidFilePath(install: UpgradeInstall): string {
  return join(install.stateRoot, "deployed-worker.pid");
}

async function withUpgradePage(
  browser: Browser,
  stack: TerminalTestStack,
  testInfo: TestInfo,
  body: (page: Page) => Promise<void>,
): Promise<void> {
  const context = await browser.newContext();
  await context.addInitScript((dashboardId) => {
    localStorage.setItem("roostSmoke", "1");
    localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
    localStorage.setItem("roost.dashboardId", dashboardId);
  }, stack.dashboardId);
  const page = await context.newPage();
  try {
    await enrollDashboardBrowser(page, stack);
    await page.waitForFunction(() => typeof window.__smoke === "object");
    await page.waitForFunction(
      (workerFp) => !!window.__smoke.state().workers[workerFp],
      stack.workerFp,
    );
    await body(page);
  } finally {
    if (testInfo.status !== testInfo.expectedStatus) {
      await testInfo.attach("worker.log", {
        body: await Bun.file(stack.workerLogPath).text(),
        contentType: "text/plain",
      }).catch(() => undefined);
    }
    await context.close();
  }
}

/** The selected pane must paint the marker itself: the proof reads the grid the
 *  renderer produced, not the wire bytes behind it. */
async function provePaintedMarker(page: Page, terminal: MarkedTerminal): Promise<void> {
  const proof = await page.evaluate(({ id, expected, timeout }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, expected, timeout);
  }, { id: terminal.sessionId, expected: terminal.marker, timeout: MARKER_PAINT_TIMEOUT_MS });
  expect(proof).toMatchObject({
    proof_kind: "marker",
    sessionId: terminal.sessionId,
    marker: terminal.marker,
  });
}
