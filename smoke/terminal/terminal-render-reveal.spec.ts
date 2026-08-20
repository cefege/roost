import { test, expect } from "./fixtures.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import { ResizeCause } from "../../apps/shared/src/gen/roost/v1/coordinator_pb.ts";
import {
  encodePtyFixtureCommand,
  PTY_FIXTURE_READY,
} from "./pty-fixture-protocol.ts";
import {
  spawnSmokeShell,
  spawnPtyFixtureSession,
  navigateToSmokeSession,
  waitForStableCellFrames,
  setRecoveryCanary,
  recoveryProbe,
} from "./terminal-helpers.ts";
import {
  readTerminalStreamProbe,
  workerViewerClaimCount,
} from "./terminal-probe-helpers.ts";

// Returning to a current warm pane is a frame-free visibility flip that keeps
// the Sync socket. A document visibility return instead reclaims one
// authoritative snapshot when the hidden grid moved; stale-link resume may
// reconnect Sync while doing so. Hidden and offscreen panes receive no cells.
test("a dormant pane stays frame-free on deck reveal and repairs document return", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop deck/visibility contract");
  const spawn = (folder: string) => smokePage.evaluate(async ({ workerFp, dir }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, dir)).session_id;
  }, { workerFp: stack.workerFp, dir: folder });

  const sessionA = await spawn("/tmp");
  const probe = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): { atBottom: boolean };
        markerScan(sessionId: string, prefix: string): { max: number; duplicated: number[]; outOfOrder: number };
        cellFrameCount(sessionId: string): number;
        cellFullFrameCount(sessionId: string): number;
        syncWsGeneration(): number;
      };
    }).__smoke;
    const scan = smoke.markerScan(id, "CELLLINE-");
    return {
      atBottom: smoke.renderProbe(id).atBottom,
      markerMax: scan.max,
      duplicated: scan.duplicated,
      outOfOrder: scan.outOfOrder,
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      wsGeneration: smoke.syncWsGeneration(),
    };
  }, sessionA);

  await smokePage.goto(`${stack.baseUrl}/s/${sessionA}`);
  const slotA = smokePage.getByTestId(`terminal-slot-${sessionA}`);
  await expect(slotA).toBeVisible();
  await smokePage.keyboard.type("seq -f 'CELLLINE-%g' 1 8000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slotA.textContent(), { timeout: 60_000 }).toContain("CELLLINE-8000");
  await smokePage.waitForTimeout(1000);

  // Deck switch with NO output on A while it is parked. A withdraws offscreen,
  // and the return claim carries a held_cell_seq the worker can prove is still
  // current, so the reveal is a visibility flip: zero cells, no repaint.
  const sessionB = await spawn("/tmp");
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { state(): { sessions: Record<string, unknown> } } }).__smoke;
    return id in smoke.state().sessions;
  }, sessionB)).toBe(true);
  const beforeSwitch = await probe();
  expect(beforeSwitch.markerMax).toBe(8000);

  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionB);
  await expect(smokePage.getByTestId(`tab-${sessionB}`)).toHaveAttribute("data-active", "true");
  await smokePage.waitForTimeout(1000);
  expect((await probe()).frames).toBe(beforeSwitch.frames);

  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionA);
  await expect(smokePage.getByTestId(`tab-${sessionA}`)).toHaveAttribute("data-active", "true");
  // Zero, not one: proving an absence needs a settle window, not a poll (a poll
  // that starts equal passes before the frame it is meant to catch could land).
  // The dwell on B was 1000 ms, past VIEWER_WITHDRAW_GRACE_MS (800), so this is
  // genuinely the unwatched path and not a claim that never stopped streaming.
  await smokePage.waitForTimeout(1000);
  expect(await probe()).toMatchObject({
    atBottom: true,
    markerMax: 8000,
    duplicated: [],
    outOfOrder: 0,
    frames: beforeSwitch.frames,
    fullFrames: beforeSwitch.fullFrames,
    wsGeneration: beforeSwitch.wsGeneration,
  });

  // Deterministic document-hidden pin: the page stays schedulable while
  // lifecycle handlers withdraw A. Output advances at the PTY but no cell
  // reaches the browser until visibility returns and one authoritative
  // snapshot reclaims it. Stale-link resume may reconnect Sync on that return.
  const beforeHide = await probe();
  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: { forceHidden(on: boolean): void } }).__smoke;
    smoke.forceHidden(true);
  });
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & { __smoke: { input(sessionId: string, text: string): Promise<void> } }).__smoke;
    await smoke.input(id, "for i in $(seq 8001 8200); do echo CELLLINE-$i; sleep 0.01; done\r");
  }, sessionA);
  await smokePage.waitForTimeout(3000);
  expect(await probe()).toMatchObject({
    markerMax: 8000,
    frames: beforeHide.frames,
    fullFrames: beforeHide.fullFrames,
    wsGeneration: beforeHide.wsGeneration,
  });

  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: { forceHidden(on: boolean): void } }).__smoke;
    smoke.forceHidden(false);
  });
  await expect.poll(async () => (await probe()).markerMax, { timeout: 60_000 }).toBe(8200);
  const afterShow = await probe();
  expect(afterShow).toMatchObject({
    atBottom: true,
    markerMax: 8200,
    duplicated: [],
    outOfOrder: 0,
    fullFrames: beforeHide.fullFrames + 1,
  });
});

test("an already-painted active pane repairs on a same-session document visibility return", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop document visibility contract");
  test.setTimeout(120_000);

  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 12);
  const originalMarker = `DOC-VIS-ORIGINAL-${suffix}`;
  const newMarker = `DOC-VIS-NEW-${suffix}`;
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);

  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  const loadingStatus = smokePage.getByTestId("terminal-loading-status");
  const readCounters = () => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    return {
      fullFrames: smoke.cellFullFrameCount(id),
      backfills: smoke.scrollbackBackfillRequestCount(id),
      lastFullFrameSbRows: smoke.lastFullFrameSbRows(id),
    };
  }, sessionId);

  await expect(slot).toBeVisible();
  const originalPaint = await smokePage.evaluate(async ({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    await smoke.input(id, `printf '%s\\n' ${marker}\\r`);
    return smoke.waitForPaintedMarker(id, marker, 15_000);
  }, { id: sessionId, marker: originalMarker });
  expect(originalPaint).toMatchObject({
    proof_kind: "marker",
    marker: originalMarker,
    frames: 2,
  });
  await expect.poll(async () => {
    const browser = (await readTerminalStreamProbe(smokePage, sessionId)).browser;
    const desired = browser.claim.desired;
    return {
      positive: (desired?.cols ?? 0) > 0 && (desired?.rows ?? 0) > 0,
      ready: browser.claim.status === "ready"
        && browser.claim.confirmed?.client_seq === desired?.client_seq,
      inLayout: browser.slot.in_layout,
      surfaceActive: browser.slot.surface_active,
    };
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toEqual({
    positive: true,
    ready: true,
    inLayout: true,
    surfaceActive: true,
  });

  const before = await readCounters();
  const beforeProbe = await readTerminalStreamProbe(smokePage, sessionId);
  const beforeDesired = beforeProbe.browser.claim.desired;
  if (!beforeDesired) throw new Error("painted terminal omitted its positive viewport claim");
  expect(beforeProbe.browser.handler_canonical).toEqual(beforeProbe.browser.dom_reconciled);
  await expect(loadingStatus).toHaveCount(0);

  const initialUrl = smokePage.url();
  const documentCanaryKey = `__roostDocumentVisibility_${suffix}`;
  const slotCanaryKey = `__roostSlotVisibility_${suffix}`;
  const canary = `visibility-${suffix}`;
  await smokePage.evaluate(({ id, documentKey, slotKey, value }) => {
    const liveSlot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    if (!(liveSlot instanceof HTMLElement)) throw new Error("active terminal slot disappeared before visibility round-trip");
    const runtime = {
      value,
      slot: liveSlot,
      loadingSeen: document.querySelector('[data-testid="terminal-loading-status"]') !== null,
      observer: null as MutationObserver | null,
    };
    const isLoadingNode = (node: Node): boolean => node instanceof Element
      && (node.matches('[data-testid="terminal-loading-status"]')
        || node.querySelector('[data-testid="terminal-loading-status"]') !== null);
    runtime.observer = new MutationObserver((records) => {
      if (records.some((record) => Array.from(record.addedNodes).some(isLoadingNode))) {
        runtime.loadingSeen = true;
      }
    });
    runtime.observer.observe(document.documentElement, { childList: true, subtree: true });
    Object.defineProperty(document, documentKey, { value: runtime, configurable: false });
    Object.defineProperty(liveSlot, slotKey, { value, configurable: false });
  }, {
    id: sessionId,
    documentKey: documentCanaryKey,
    slotKey: slotCanaryKey,
    value: canary,
  });

  await smokePage.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.forceHidden(true);
  });
  await expect.poll(async () => {
    const browser = (await readTerminalStreamProbe(smokePage, sessionId)).browser;
    const desired = browser.claim.desired;
    return {
      desired: desired && {
        cols: desired.cols,
        rows: desired.rows,
        cause: desired.cause,
      },
      sequenceAdvanced: desired !== null
        && BigInt(desired.client_seq) > BigInt(beforeDesired.client_seq),
      confirmed: desired !== null
        && browser.claim.confirmed?.client_seq === desired.client_seq,
      inLayout: browser.slot.in_layout,
      surfaceActive: browser.slot.surface_active,
      pageVisible: browser.visibility.page_visible,
    };
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toEqual({
    desired: { cols: 0, rows: 0, cause: ResizeCause.WITHDRAW },
    sequenceAdvanced: true,
    confirmed: true,
    inLayout: true,
    surfaceActive: true,
    pageVisible: false,
  });
  const hiddenProbe = await readTerminalStreamProbe(smokePage, sessionId);
  const hiddenDesired = hiddenProbe.browser.claim.desired;
  if (!hiddenDesired) throw new Error("hidden terminal omitted its withdraw claim");
  const hiddenCounters = await readCounters();
  expect(hiddenCounters.fullFrames).toBe(before.fullFrames);
  expect(hiddenCounters.backfills).toBe(before.backfills);

  await smokePage.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.forceHidden(false);
  });
  await expect.poll(async () => {
    const browser = (await readTerminalStreamProbe(smokePage, sessionId)).browser;
    const desired = browser.claim.desired;
    return {
      positive: (desired?.cols ?? 0) > 0 && (desired?.rows ?? 0) > 0,
      cause: desired?.cause ?? null,
      heldCellSeq: desired?.held_cell_seq ?? null,
      sequenceAdvanced: desired !== null
        && BigInt(desired.client_seq) > BigInt(hiddenDesired.client_seq),
      inLayout: browser.slot.in_layout,
      surfaceActive: browser.slot.surface_active,
      pageVisible: browser.visibility.page_visible,
    };
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toEqual({
    positive: true,
    cause: ResizeCause.TAB_VISIBLE,
    heldCellSeq: "0",
    sequenceAdvanced: true,
    inLayout: true,
    surfaceActive: true,
    pageVisible: true,
  });
  await expect.poll(
    async () => (await readCounters()).fullFrames,
    { timeout: 15_000, intervals: [25, 50, 100, 250] },
  ).toBe(before.fullFrames + 1);
  await expect.poll(async () => {
    const claim = (await readTerminalStreamProbe(smokePage, sessionId)).browser.claim;
    return claim.status === "ready"
      && claim.desired !== null
      && claim.confirmed?.client_seq === claim.desired.client_seq;
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toBe(true);

  const originalRepaint = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 15_000);
  }, { id: sessionId, marker: originalMarker });
  expect(originalRepaint).toMatchObject({
    proof_kind: "marker",
    marker: originalMarker,
    frames: 2,
  });
  const newPaint = await smokePage.evaluate(async ({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    await smoke.input(id, `printf '%s\\n' ${marker}\\r`);
    return smoke.waitForPaintedMarker(id, marker, 15_000);
  }, { id: sessionId, marker: newMarker });
  expect(newPaint).toMatchObject({
    proof_kind: "marker",
    marker: newMarker,
    frames: 2,
  });

  const after = await readCounters();
  expect(after.fullFrames - before.fullFrames).toBe(1);
  expect(after.backfills - before.backfills).toBe(0);
  expect(after.lastFullFrameSbRows).toBe(0);
  const afterProbe = await readTerminalStreamProbe(smokePage, sessionId);
  expect(afterProbe.browser.claim.status).toBe("ready");
  expect(afterProbe.browser.claim.confirmed?.client_seq)
    .toBe(afterProbe.browser.claim.desired?.client_seq);
  expect(afterProbe.browser.handler_canonical).toEqual(afterProbe.browser.dom_reconciled);
  expect(afterProbe.browser.slot).toMatchObject({
    in_layout: true,
    surface_active: true,
  });

  const survival = await smokePage.evaluate(({ id, documentKey, slotKey, value }) => {
    const liveSlot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const documentRecord = document as unknown as Record<string, unknown>;
    const runtime = documentRecord[documentKey] as {
      value: string;
      slot: Element;
      loadingSeen: boolean;
      observer: MutationObserver;
    } | undefined;
    let slotSurvived = false;
    if (liveSlot !== null) {
      const slotRecord = liveSlot as unknown as Record<string, unknown>;
      slotSurvived = runtime?.slot === liveSlot && slotRecord[slotKey] === value;
    }
    runtime?.observer.disconnect();
    return {
      document: runtime?.value === value,
      slot: slotSurvived,
      loadingSeen: runtime?.loadingSeen ?? true,
    };
  }, {
    id: sessionId,
    documentKey: documentCanaryKey,
    slotKey: slotCanaryKey,
    value: canary,
  });
  expect(survival).toEqual({
    document: true,
    slot: true,
    loadingSeen: false,
  });
  await expect(loadingStatus).toHaveCount(0);
  expect(smokePage.url()).toBe(initialUrl);
});

test("a document-return repair waits for a synchronized TUI repaint boundary", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop synchronized-output contract");
  test.setTimeout(90_000);

  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 12);
  const staticMarker = `SYNC-STATIC-${suffix}`;
  const dynamicMarker = `SYNC-DYNAMIC-${suffix}`;
  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
  await navigateToSmokeSession(smokePage, sessionId);
  await smokePage.evaluate(async ({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 30_000);
  }, { id: sessionId, marker: PTY_FIXTURE_READY });
  const readCounters = () => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    return {
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      backfills: smoke.scrollbackBackfillRequestCount(id),
      lastFullFrameSbRows: smoke.lastFullFrameSbRows(id),
    };
  }, sessionId);
  const workerRawHead = async (): Promise<number> => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    const raw = probe.worker.session?.raw;
    if (raw === null || typeof raw !== "object" || !("head_seq" in raw)) return -1;
    const headSeq = raw.head_seq;
    return typeof headSeq === "number" || typeof headSeq === "string"
      ? Number(headSeq)
      : -1;
  };
  await expect.poll(async () => {
    const counters = await readCounters();
    return counters.fullFrames;
  }).toBeGreaterThan(0);
  await waitForStableCellFrames(smokePage, sessionId);

  await smokePage.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.forceHidden(true);
  });
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    const desired = probe.browser.claim.desired;
    return {
      cause: desired?.cause ?? null,
      confirmed: desired !== null
        && probe.browser.claim.confirmed?.client_seq === desired.client_seq,
      workerClaims: workerViewerClaimCount(probe),
    };
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toEqual({
    cause: ResizeCause.WITHDRAW,
    confirmed: true,
    workerClaims: 0,
  });

  const hiddenRaw = await workerRawHead();
  const before = await readCounters();
  const firstHalf = encodePtyFixtureCommand({
    op: "EMIT",
    newline: false,
    text: `\x1b[?2026h\x1b[2J\x1b[H${staticMarker}`,
  });
  await smokePage.evaluate(async ({ id, command }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, command);
  }, { id: sessionId, command: firstHalf });
  await expect.poll(workerRawHead, {
    timeout: 15_000,
    intervals: [25, 50, 100],
  }).toBeGreaterThan(hiddenRaw);
  expect(await readCounters()).toEqual(before);

  await smokePage.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.forceHidden(false);
  });
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    const desired = probe.browser.claim.desired;
    const syncOutput = probe.worker.session?.sync_output;
    const counters = await readCounters();
    return {
      cause: desired?.cause ?? null,
      heldCellSeq: desired?.held_cell_seq ?? null,
      confirmed: desired !== null
        && probe.browser.claim.confirmed?.client_seq === desired.client_seq,
      syncHeld: syncOutput !== null
        && typeof syncOutput === "object"
        && "tripped" in syncOutput
        && syncOutput.tripped === false,
      frames: counters.frames,
      fullFrames: counters.fullFrames,
    };
  }, { timeout: 15_000, intervals: [10, 25, 50] }).toEqual({
    cause: ResizeCause.TAB_VISIBLE,
    heldCellSeq: "0",
    confirmed: true,
    syncHeld: true,
    frames: before.frames,
    fullFrames: before.fullFrames,
  });

  const secondHalf = encodePtyFixtureCommand({
    op: "EMIT",
    newline: false,
    text: `\x1b[2;1H${dynamicMarker}\x1b[?2026l`,
  });
  await smokePage.evaluate(async ({ id, command }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, command);
  }, { id: sessionId, command: secondHalf });
  await expect.poll(readCounters, { timeout: 15_000, intervals: [25, 50, 100] }).toMatchObject({
    frames: before.frames + 1,
    fullFrames: before.fullFrames + 1,
  });

  const staticPaint = await smokePage.evaluate(async ({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 15_000);
  }, { id: sessionId, marker: staticMarker });
  const dynamicPaint = await smokePage.evaluate(async ({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 15_000);
  }, { id: sessionId, marker: dynamicMarker });
  expect(staticPaint).toMatchObject({ proof_kind: "marker", marker: staticMarker, frames: 2 });
  expect(dynamicPaint).toMatchObject({ proof_kind: "marker", marker: dynamicMarker, frames: 2 });

  const after = await readCounters();
  expect(after).toMatchObject({
    frames: before.frames + 1,
    fullFrames: before.fullFrames + 1,
    backfills: before.backfills,
    lastFullFrameSbRows: 0,
  });
  const afterProbe = await readTerminalStreamProbe(smokePage, sessionId);
  expect(afterProbe.worker.session?.sync_output ?? null).toBeNull();
  expect(afterProbe.browser.handler_canonical).toEqual(afterProbe.browser.dom_reconciled);
});

test("offline producer divergence reconnects and repaints without a reload", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop transport recovery contract");
  test.setTimeout(90_000);
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    sessionId,
  )).toBeGreaterThan(0);
  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(
      id,
      "printf 'OFFLINE-READY-%03d\\n' 1\r",
    ),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).toContain("OFFLINE-READY-001");
  await waitForStableCellFrames(smokePage, sessionId);

  const canary = `offline-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);
  const before = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    return {
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      gridEpoch: smoke.cellGridEpoch(id),
      wsGeneration: smoke.syncWsGeneration(),
    };
  }, sessionId);

  const context = smokePage.context();
  await smokePage.evaluate(
    () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.pauseSyncTransport(),
  );
  try {
    await context.setOffline(true);
    await expect.poll(() => smokePage.evaluate(() => navigator.onLine)).toBe(false);
    await stack.client.sessionsInput({
      sessionId,
      data: new TextEncoder().encode(
        "for i in $(seq 1 30); do printf 'OFFLINE-RECOVER-%03d\\n' \"$i\"; sleep 0.01; done; seq 1 48; printf 'OFFLINE-CURRENT-%03d\\n' 1\r",
      ),
    });
    await expect.poll(async () => {
      const cells = await stack.client.sessionsGetScrollbackCells({
        sessionId,
        endRow: BigInt(Number.MAX_SAFE_INTEGER),
        maxRows: 250,
        gridEpoch: before.gridEpoch,
      });
      const text = cells.rows
        .map((row) => row.spans.map((span) => span.text || " ").join(""))
        .join("\n");
      return Math.max(0, ...Array.from(text.matchAll(/OFFLINE-RECOVER-(\d+)/g), (match) => Number(match[1])));
    }, { timeout: 30_000, intervals: [100] }).toBe(30);
    const isolated = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
      return {
        frames: smoke.cellFrameCount(id),
        max: smoke.markerScan(id, "OFFLINE-RECOVER-").max,
      };
    }, sessionId);
    expect(isolated).toEqual({ frames: before.frames, max: 0 });
  } finally {
    await context.setOffline(false);
    await smokePage.evaluate(
      () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.resumeSyncTransport(),
    );
  }

  await expect.poll(() => smokePage.evaluate(
    () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  ), { timeout: 30_000, intervals: [100] }).toBeGreaterThan(before.wsGeneration);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  ), { timeout: 30_000, intervals: [100] }).toContain("OFFLINE-CURRENT-001");

  const recovered = await recoveryProbe(smokePage, sessionId, "OFFLINE-RECOVER-");
  expect(recovered).toMatchObject({
    canary,
    atBottom: true,
    scan: { max: 0, duplicated: [], missing: 0, outOfOrder: 0 },
  });
  expect(await smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.lastFullFrameSbRows(id),
    sessionId,
  )).toBe(0);
  const afterReconnect = await smokePage.evaluate((id) => ({
    fullFrames: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    wsGeneration: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  }), sessionId);
  expect(afterReconnect.fullFrames).toBe(before.fullFrames + 1);

  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(
      id,
      "printf 'OFFLINE-AFTER-%03d\\n' 1\r",
    ),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).toContain("OFFLINE-AFTER-001");
  expect(await smokePage.evaluate(
    () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  )).toBe(afterReconnect.wsGeneration);
});

// Bottom-follow must survive geometry changes that happen while a pane is
// parked: the box shrinks (window resize / keyboard inset / divider drag),
// nothing re-samples the bottom, and pre-noteBoxResize the pane revealed
// off-bottom with live output landing below the fold — permanently.
test("a pane revealed after the window shrank is still at the bottom", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop scroll-geometry contract");
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'SHRK-%g' 1 600");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 30_000 }).toContain("SHRK-600");

  // Park A behind a sibling, then shrink the window UNDER the parked pane.
  const siblingId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, siblingId);
  await expect(smokePage.getByTestId(`tab-${siblingId}`)).toHaveAttribute("data-active", "true");
  const vp = smokePage.viewportSize()!;
  // −100 px keeps the short side ≥ 600 (windowSizeClass compact boundary keys
  // on min(w,h)): the pane must SHRINK, not flip the whole app to mobile UI.
  await smokePage.setViewportSize({ width: vp.width, height: vp.height - 100 });
  await smokePage.waitForTimeout(400); // park restyle + ResizeObserver tick

  // Reveal: the FIRST sample with painted rows is already at the bottom.
  await smokePage.evaluate((id) => {
    const w = window as unknown as Window & { __shrkSamples?: Array<{ painted: number; top: number; height: number; client: number }>; __shrkTimer?: number };
    const slotEl = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slotEl?.querySelector(".wterm") as HTMLElement | null;
    const samples: Array<{ painted: number; top: number; height: number; client: number }> = [];
    w.__shrkSamples = samples;
    w.__shrkTimer = window.setInterval(() => {
      if (!c) return;
      samples.push({
        painted: c.querySelectorAll(".cell-row").length,
        top: c.scrollTop, height: c.scrollHeight, client: c.clientHeight,
      });
    }, 50);
  }, sessionId);
  await smokePage.getByTestId(`tab-${sessionId}`).click();
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { renderProbe(sessionId: string): { atBottom: boolean; rowCount: number } };
    }).__smoke;
    const p = smoke.renderProbe(id);
    return p.rowCount > 0 && p.atBottom;
  }, sessionId), { timeout: 1_500, intervals: [50] }).toBe(true);
  await smokePage.waitForTimeout(1_000);
  const samples = await smokePage.evaluate(() => {
    const w = window as unknown as Window & { __shrkSamples?: Array<{ painted: number; top: number; height: number; client: number }>; __shrkTimer?: number };
    if (w.__shrkTimer !== undefined) window.clearInterval(w.__shrkTimer);
    return w.__shrkSamples ?? [];
  });
  expect(samples.length).toBeGreaterThan(5);
  const offBottom = samples.filter((s) => s.painted > 0 && s.top < s.height - s.client - 2);
  expect(offBottom).toEqual([]);
  // The newest marker is visible at the bottom of the SHRUNKEN box.
  const lastVisible = await smokePage.evaluate((id) => {
    const slotEl = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slotEl?.querySelector(".wterm") as HTMLElement | null;
    if (!c) return -1;
    const box = c.getBoundingClientRect();
    let max = -1;
    for (const row of c.querySelectorAll(".cell-row")) {
      const r = row.getBoundingClientRect();
      if (r.bottom <= box.top + 1 || r.top >= box.bottom - 1) continue;
      const m = (row.textContent ?? "").match(/SHRK-(\d+)/);
      if (m) max = Math.max(max, Number(m[1]));
    }
    return max;
  }, sessionId);
  expect(lastVisible).toBe(600);
});
