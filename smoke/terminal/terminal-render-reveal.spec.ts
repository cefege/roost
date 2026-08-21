import { test, expect } from "./fixtures.ts";
import {
  coordinatorTerminalViewState,
  readTerminalStreamProbe,
} from "./terminal-probe-helpers.ts";

// Every inactive-to-active transition establishes a complete terminal baseline.
// The previously painted DOM stays visible while that baseline arrives, and
// hidden/offscreen panes receive no cells in the meantime.
test("a dormant pane rebaselines on deck and document return", async ({ smokePage, stack }, testInfo) => {
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

  // Deck switch with NO output on A while it is inactive. Its previous replica
  // and DOM stay painted, but returning still installs one complete baseline
  // under the fresh coordinator stream.
  const sessionB = await spawn("/tmp");
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { state(): { sessions: Record<string, unknown> } } }).__smoke;
    return id in smoke.state().sessions;
  }, sessionB)).toBe(true);
  const beforeSwitch = await probe();
  expect(beforeSwitch.markerMax).toBe(8000);
  const beforeSwitchStream = await readTerminalStreamProbe(smokePage, sessionA);
  const initialStreamId = beforeSwitchStream.browser.view.stream_id;
  if (!initialStreamId) throw new Error("dormant-pane fixture omitted its initial stream");

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
  await expect.poll(async () => {
    const stream = await readTerminalStreamProbe(smokePage, sessionA);
    const coordinator = coordinatorTerminalViewState(stream);
    return stream.browser.view.status === "accepted"
      && stream.browser.view.active
      && stream.browser.replica.baseline_ready
      && stream.browser.replica.expected_stream_id === stream.browser.view.stream_id
      && coordinator?.activeViews === 1
      && coordinator.streamId === stream.browser.view.stream_id
      && coordinator.streamId !== initialStreamId;
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toBe(true);
  // Let any later transport delivery settle before asserting the exact
  // rebaseline count.
  await smokePage.waitForTimeout(1000);
  expect(await probe()).toMatchObject({
    atBottom: true,
    markerMax: 8000,
    duplicated: [],
    outOfOrder: 0,
    frames: beforeSwitch.frames + 1,
    fullFrames: beforeSwitch.fullFrames + 1,
    wsGeneration: beforeSwitch.wsGeneration,
  });

  // Deterministic document-hidden pin: the page stays schedulable while its
  // lifecycle handler marks A inactive. Output advances at the PTY but no cell
  // reaches the browser until visibility returns and a fresh-stream baseline
  // installs. Stale-link resume may reconnect Sync on that return.
  const beforeHide = await probe();
  const beforeHideStream = await readTerminalStreamProbe(smokePage, sessionA);
  const beforeHideStreamId = beforeHideStream.browser.view.stream_id;
  if (!beforeHideStreamId) throw new Error("document-hide fixture omitted its active stream");
  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: { forceHidden(on: boolean): void } }).__smoke;
    smoke.forceHidden(true);
  });
  await expect.poll(async () => {
    const stream = await readTerminalStreamProbe(smokePage, sessionA);
    const coordinator = coordinatorTerminalViewState(stream);
    return {
      status: stream.browser.view.status,
      active: stream.browser.view.active,
      coordinatorViews: coordinator?.activeViews ?? -1,
      coordinatorGeometry: coordinator?.effective,
    };
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toEqual({
    status: "accepted",
    active: false,
    coordinatorViews: 0,
    coordinatorGeometry: null,
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
  await expect.poll(async () => {
    const stream = await readTerminalStreamProbe(smokePage, sessionA);
    const coordinator = coordinatorTerminalViewState(stream);
    return stream.browser.view.status === "accepted"
      && stream.browser.view.active
      && stream.browser.view.stream_id !== beforeHideStreamId
      && stream.browser.replica.baseline_ready
      && stream.browser.replica.expected_stream_id === stream.browser.view.stream_id
      && coordinator?.activeViews === 1
      && coordinator.streamId === stream.browser.view.stream_id;
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toBe(true);
  const afterShow = await probe();
  expect(afterShow).toMatchObject({
    atBottom: true,
    markerMax: 8200,
    duplicated: [],
    outOfOrder: 0,
    fullFrames: beforeHide.fullFrames + 1,
  });
});
