import { test, expect } from "./fixtures.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  spawnSmokeShell,
  navigateToSmokeSession,
} from "./terminal-helpers.ts";
import {
  coordinatorTerminalViewState,
  readTerminalStreamProbe,
} from "./terminal-probe-helpers.ts";

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
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    const { view, replica } = probe.browser;
    const coordinator = coordinatorTerminalViewState(probe);
    return {
      status: view.status,
      active: view.active,
      positive: view.effective_cols !== null && view.effective_rows !== null
        && view.effective_cols > 0 && view.effective_rows > 0,
      baselineReady: replica.baseline_ready,
      streamMatched: replica.expected_stream_id === view.stream_id
        && coordinator?.streamId === view.stream_id,
      coordinatorViews: coordinator?.activeViews ?? 0,
      coordinatorGeometryMatched: coordinator?.effective?.cols === view.effective_cols
        && coordinator.effective?.rows === view.effective_rows,
      inLayout: probe.browser.slot.in_layout,
      surfaceActive: probe.browser.slot.surface_active,
    };
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toEqual({
    status: "accepted",
    active: true,
    positive: true,
    baselineReady: true,
    streamMatched: true,
    coordinatorViews: 1,
    coordinatorGeometryMatched: true,
    inLayout: true,
    surfaceActive: true,
  });

  const before = await readCounters();
  const beforeProbe = await readTerminalStreamProbe(smokePage, sessionId);
  const beforeView = beforeProbe.browser.view;
  if (!beforeView.revision || !beforeView.stream_id) {
    throw new Error("painted terminal omitted its accepted active view");
  }
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
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    const { view } = probe.browser;
    const coordinator = coordinatorTerminalViewState(probe);
    return {
      status: view.status,
      active: view.active,
      zeroGeometry: view.effective_cols === 0 && view.effective_rows === 0,
      revisionAdvanced: view.revision !== null
        && BigInt(view.revision) > BigInt(beforeView.revision),
      streamCleared: view.stream_id === "",
      coordinatorViews: coordinator?.activeViews ?? -1,
      coordinatorGeometry: coordinator?.effective,
      inLayout: probe.browser.slot.in_layout,
      surfaceActive: probe.browser.slot.surface_active,
      pageVisible: probe.browser.visibility.page_visible,
    };
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toEqual({
    status: "accepted",
    active: false,
    zeroGeometry: true,
    revisionAdvanced: true,
    streamCleared: true,
    coordinatorViews: 0,
    coordinatorGeometry: null,
    inLayout: true,
    surfaceActive: true,
    pageVisible: false,
  });
  const hiddenProbe = await readTerminalStreamProbe(smokePage, sessionId);
  const hiddenRevision = hiddenProbe.browser.view.revision;
  if (!hiddenRevision) throw new Error("hidden terminal omitted its inactive view revision");
  const hiddenCounters = await readCounters();
  expect(hiddenCounters.fullFrames).toBe(before.fullFrames);
  expect(hiddenCounters.backfills).toBe(before.backfills);

  await smokePage.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.forceHidden(false);
  });
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    const { view, replica } = probe.browser;
    const coordinator = coordinatorTerminalViewState(probe);
    return {
      status: view.status,
      active: view.active,
      positive: view.effective_cols !== null && view.effective_rows !== null
        && view.effective_cols > 0 && view.effective_rows > 0,
      revisionAdvanced: view.revision !== null
        && BigInt(view.revision) > BigInt(hiddenRevision),
      newStream: view.stream_id !== null
        && view.stream_id !== ""
        && view.stream_id !== beforeView.stream_id,
      baselineReady: replica.baseline_ready,
      replicaStreamMatched: replica.expected_stream_id === view.stream_id,
      coordinatorViews: coordinator?.activeViews ?? 0,
      coordinatorStreamMatched: coordinator?.streamId === view.stream_id,
      coordinatorGeometryMatched: coordinator?.effective?.cols === view.effective_cols
        && coordinator.effective?.rows === view.effective_rows,
      inLayout: probe.browser.slot.in_layout,
      surfaceActive: probe.browser.slot.surface_active,
      pageVisible: probe.browser.visibility.page_visible,
    };
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toEqual({
    status: "accepted",
    active: true,
    positive: true,
    revisionAdvanced: true,
    newStream: true,
    baselineReady: true,
    replicaStreamMatched: true,
    coordinatorViews: 1,
    coordinatorStreamMatched: true,
    coordinatorGeometryMatched: true,
    inLayout: true,
    surfaceActive: true,
    pageVisible: true,
  });
  await expect.poll(
    async () => (await readCounters()).fullFrames,
    { timeout: 15_000, intervals: [25, 50, 100, 250] },
  ).toBe(before.fullFrames + 1);

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
  expect(afterProbe.browser.view).toMatchObject({
    status: "accepted",
    active: true,
  });
  expect(afterProbe.browser.replica).toMatchObject({
    expected_stream_id: afterProbe.browser.view.stream_id,
    baseline_ready: true,
    resync_latched: false,
  });
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
    loadingSeen: true,
  });
  await expect(loadingStatus).toHaveCount(0);
  expect(smokePage.url()).toBe(initialUrl);
});
