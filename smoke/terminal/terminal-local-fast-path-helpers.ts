// Support for the worker-served local terminal proof: the enrolled browsers it
// pairs against this stack's two origins, the armed keystroke→paint clock they
// are compared on, and the browser/worker readings that name which transport a
// pane's frames actually travelled.
// Called only by terminal-local-fast-path.spec.ts; depends on the tier's stack
// module, window.__smoke, and the coordinator's own keeper-row reader.

import type { Browser, BrowserContext, Page, TestInfo } from "@playwright/test";
import { workerInventoryForUpdateAdmission } from "../../apps/roost-cli/src/status-report.ts";
import type { KeeperRuntimeObservationV1 } from "../../apps/shared/src/keeper-update.ts";
import { installDisabledLoopbackProbe } from "./stack-browser-faults.ts";
import { enrollSmokeBrowser } from "./fixtures.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import { logTail, waitFor } from "./stack-runtime.ts";
import type { TerminalTestStack } from "./stack.ts";
import { unknownRecord } from "./terminal-probe-helpers.ts";

/** A worker beats every 30s, so a keeper row can take that long to move. */
const KEEPER_OBSERVATION_TIMEOUT_MS = 90_000;

export interface EnrolledPage {
  page: Page;
  context: BrowserContext;
  origin: string;
  close(): Promise<void>;
}

/** Browser-local terminal facts that survive a coordinator outage: no RPC, so
 * this is the only transport reading available while coord is down. The
 * accepted frame token is compared with the elected route rather than inferred
 * from its epoch. */
export interface LocalTransportReading {
  acceptedTransportKind: "sync" | "loopback" | "webrtc" | null;
  acceptedWorkerEpoch: string | null;
  electedTransportKind: "sync" | "loopback" | "webrtc" | null;
  electedWorkerEpoch: string | null;
  tokenMatchesElectedRoute: boolean;
  viewStatus: string | null;
  viewStreamId: string | null;
  wireStreamId: string | null;
  baselineReady: boolean;
  syncReady: boolean;
}

export interface WorkerCellDelivery {
  sinkId: string;
  active: boolean;
  baselineReady: boolean;
}

/**
 * Pair a fresh browser against one origin of this stack: the coordinator's own
 * door, or the loopback door a worker serves. Each origin is a DISTINCT device
 * (its own IndexedDB key), so each redeems its own bootstrap token through the
 * tier's ordinary enrollment — only `baseUrl` differs, and everything after the
 * redeem (worker routable, folder list painted, no error boundary) is the same
 * readiness gate every smoke page crosses.
 * `localWorkerOrigin` seeds the SPA's non-default-door override, which is how a
 * coordinator-origin page reaches a harness worker's reserved loopback port.
 */
export async function openEnrolledPage(
  browser: Browser,
  stack: TerminalTestStack,
  origin: string,
  options: { localWorkerOrigin?: string } = {},
): Promise<EnrolledPage> {
  const context = await browser.newContext();
  if (stack.disableLoopbackProbe && !options.localWorkerOrigin) {
    await installDisabledLoopbackProbe(context);
  }
  await context.addInitScript((localWorkerOrigin: string | undefined) => {
    if (localWorkerOrigin) localStorage.setItem("roost.localWorkerOrigin", localWorkerOrigin);
    localStorage.setItem("roostSmoke", "1");
    localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
    if (!sessionStorage.getItem("roost.sidebarViewSeeded")) {
      localStorage.setItem("roost.sidebarView", "folders");
      localStorage.setItem("roost.sidebarCollapsed", "0");
      sessionStorage.setItem("roost.sidebarViewSeeded", "1");
    }
  }, options.localWorkerOrigin);
  const page = await context.newPage();
  try {
    await enrollSmokeBrowser(page, { ...stack, baseUrl: origin }, stack.client);
  } catch (error) {
    await context.close();
    throw error;
  }
  return {
    page,
    context,
    origin,
    close: async () => {
      await page.evaluate(async () => {
        const smoke = window.__smoke;
        smoke?.forceVisible(false);
        await smoke?.cleanupCreated();
      }).catch(() => undefined);
      await context.close();
    },
  };
}

/** Service logs for a stack this spec owns outright. The shared `stack`
 *  fixture attaches these itself; a privately started one has no fixture to do
 *  it, and a CI failure without them is undiagnosable. */
export async function attachStackLogs(
  testInfo: TestInfo,
  stack: TerminalTestStack,
): Promise<void> {
  const logs: ReadonlyArray<readonly [string, string]> = [
    ["coord.log", stack.coordLogPath],
    ["worker.log", stack.workerLogPath],
    ["pty-fixture-worker.log", stack.ptyFixtureWorkerLogPath],
  ];
  for (const [name, path] of logs) {
    await testInfo.attach(name, { body: logTail(path), contentType: "text/plain" });
  }
}

export function readLocalTransportReading(
  page: Page,
  sessionId: string,
): Promise<LocalTransportReading> {
  return page.evaluate((id) => {
    const snapshot = window.__smoke.terminalBrowserSnapshot(id);
    const token = snapshot.replica.last_terminal_proof_generation;
    const route = snapshot.route.active;
    const acceptedTransportKind = token?.transportKind ?? null;
    const acceptedWorkerEpoch = acceptedTransportKind === "sync"
      ? null
      : token?.processEpoch ?? null;
    const electedTransportKind = route?.kind ?? null;
    const electedWorkerEpoch = route?.worker_epoch ?? null;
    return {
      acceptedTransportKind,
      acceptedWorkerEpoch,
      electedTransportKind,
      electedWorkerEpoch,
      tokenMatchesElectedRoute: acceptedTransportKind !== null
        && acceptedTransportKind === electedTransportKind
        && acceptedWorkerEpoch === electedWorkerEpoch,
      viewStatus: snapshot.view.status,
      viewStreamId: snapshot.view.stream_id,
      wireStreamId: snapshot.wire_received.stream_id,
      baselineReady: snapshot.replica.baseline_ready,
      syncReady: snapshot.sync.ready,
    };
  }, sessionId);
}

/**
 * Arm the product's own geometric paint proof BEFORE the marker can exist, so
 * the epoch it returns is when the row was really presented rather than when a
 * late poll noticed it. Reading it later resolves that same promise.
 */
export async function armPaintedMarkerEpoch(
  page: Page,
  sessionId: string,
  marker: string,
  timeoutMs = 30_000,
): Promise<void> {
  await page.evaluate(({ id, text, budget }) => {
    const runtimeWindow = window as unknown as Window & { __localFastPathPaint?: Promise<number> };
    runtimeWindow.__localFastPathPaint = window.__smoke
      .waitForPaintedMarker(id, text, budget)
      .then((proof) => proof.epochMs);
  }, { id: sessionId, text: marker, budget: timeoutMs });
}

export function readArmedPaintedMarkerEpoch(page: Page): Promise<number> {
  return page.evaluate(() => {
    const runtimeWindow = window as unknown as Window & { __localFastPathPaint?: Promise<number> };
    const pending = runtimeWindow.__localFastPathPaint;
    if (!pending) throw new Error("no armed painted-marker waiter on this document");
    return pending;
  });
}

/**
 * Emit one marker line from this page's pane and report the epoch the page
 * released it at. Both origins run in one browser on one host, so the two
 * pages' Date.now() and the paint proof's epochMs share a clock.
 */
export function emitMarkerFromPage(
  page: Page,
  sessionId: string,
  marker: string,
): Promise<number> {
  const payload = encodePtyFixtureCommand({ op: "EMIT", text: marker, newline: true });
  return page.evaluate(async ({ id, command }) => {
    const sentAtEpochMs = Date.now();
    await window.__smoke.input(id, command);
    return sentAtEpochMs;
  }, { id: sessionId, command: payload });
}

/** Per-sink cell delivery bookkeeping the worker reports for a session's live
 *  stream, plus the stream id the worker itself holds. */
export function workerStreamDeliveries(
  workerSession: Record<string, unknown> | null,
): { streamId: string; deliveries: WorkerCellDelivery[] } | null {
  const stream = unknownRecord(workerSession?.terminal_stream);
  if (!stream) return null;
  const streamId = stream.stream_id;
  if (typeof streamId !== "string" || streamId.length === 0) {
    throw new Error("worker terminal stream reported no stream id");
  }
  const rows = stream.deliveries;
  if (!Array.isArray(rows)) throw new Error("worker terminal stream reported no deliveries");
  return {
    streamId,
    deliveries: rows.map((entry, index) => {
      const record = unknownRecord(entry);
      if (!record) throw new Error(`worker cell delivery ${index} was not an object`);
      const sinkId = record.sink_id;
      if (typeof sinkId !== "string" || sinkId.length === 0) {
        throw new Error(`worker cell delivery ${index} reported no sink id`);
      }
      if (typeof record.active !== "boolean" || typeof record.baseline_ready !== "boolean") {
        throw new Error(`worker cell delivery ${index} had non-boolean state`);
      }
      return { sinkId, active: record.active, baselineReady: record.baseline_ready };
    }),
  };
}

export interface KeeperRow {
  runtime: KeeperRuntimeObservationV1;
  lastSeenMs: number;
}

/**
 * The coordinator's keeper observation for one worker, read from the database
 * the product's own update admission reads. Only a heartbeat moves it, so a
 * caller comparing across a coordinator bounce must wait for a fresher row.
 */
export function keeperRow(stack: TerminalTestStack, workerFp: string): KeeperRow {
  const matches = workerInventoryForUpdateAdmission(stack.coordDbPath)
    .filter((worker) => worker.fingerprint === workerFp);
  if (matches.length !== 1) {
    throw new Error(`coordinator database holds ${matches.length} rows for worker ${workerFp}`);
  }
  const row = matches[0]!;
  if (!row.keeperRuntime) {
    throw new Error(`${row.label}: coordinator holds no keeper runtime observation`);
  }
  return { runtime: row.keeperRuntime, lastSeenMs: row.lastSeenMs };
}

/** A keeper row from a heartbeat the worker sent AFTER `sinceLastSeenMs`, so a
 *  post-bounce comparison reads what the reconnected worker reported rather
 *  than the row the outage froze. Pass 0 for the first observation of all. */
export function waitForKeeperRowAfter(
  stack: TerminalTestStack,
  workerFp: string,
  sinceLastSeenMs: number,
): Promise<KeeperRow> {
  return waitFor("keeper runtime observation after reconnect", KEEPER_OBSERVATION_TIMEOUT_MS, () => {
    try {
      const row = keeperRow(stack, workerFp);
      return row.lastSeenMs > sinceLastSeenMs ? row : undefined;
    } catch {
      return undefined;
    }
  });
}

/** A keeper row that already accounts for `channels` live PTYs. A worker's
 *  first beat lands before its keeper reconciles the channel, so a baseline
 *  taken without this waits on a row that still says zero. */
export function waitForKeeperChannels(
  stack: TerminalTestStack,
  workerFp: string,
  channels: number,
): Promise<KeeperRow> {
  return waitFor(`keeper reporting ${channels} channel(s)`, KEEPER_OBSERVATION_TIMEOUT_MS, () => {
    try {
      const row = keeperRow(stack, workerFp);
      return row.runtime.channel_count === channels ? row : undefined;
    } catch {
      return undefined;
    }
  });
}
