import { test, expect } from "./fixtures.ts";
import { encodePtyFixtureCommand, PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import { inputSmokeTerminal, navigateToSmokeSession, spawnPtyFixtureSession } from "./terminal-helpers.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";
import {
  ACTIVITY_COL, FIRST_INPUT_ROW, NARROW_TALL_VIEWPORT, RESIZED_NARROW_VIEWPORT,
  RESIZED_WIDE_VIEWPORT, SECOND_INPUT_ROW, WIDE_SHORT_VIEWPORT, applyNumericUpdate,
  expectHistoryAnchorPreserved, expectMarkersOnce, expectReaderReframeHeld,
  expectedMinimum, forceHidden, forceVisible, htopFrame, integer, nonEmptyString,
  numericUpdate, readBackfillRequestCount, readPaintedScrollback, viewportText,
  waitForHistoryAnchor, waitForPainted, waitForTransition,
} from "./terminal-multiview-helpers.ts";


test("independent browsers share one continuous terminal replica and effective geometry", async ({
  smokePage: widePage,
  secondSmokePage: narrowPage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop multi-browser terminal contract");
  test.setTimeout(240_000);

  expect(widePage.context()).not.toBe(narrowPage.context());
  await Promise.all([
    widePage.setViewportSize(WIDE_SHORT_VIEWPORT),
    narrowPage.setViewportSize(NARROW_TALL_VIEWPORT),
    forceVisible(widePage, true),
    forceVisible(narrowPage, true),
  ]);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(widePage, fixtureWorker);
  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase();
  const pages = [widePage, narrowPage] as const;

  // Measure each independent viewport as the sole active viewer. Their axes are
  // deliberately crossed: the wide browser is shorter, the narrow browser is
  // taller. The two-view SCD must therefore take one effective axis from each.
  await navigateToSmokeSession(narrowPage, sessionId);
  await waitForPainted(narrowPage, sessionId, PTY_FIXTURE_READY);
  const narrowSolo = await waitForTransition([narrowPage], sessionId, {
    activeIndices: [0],
    activeViewCount: 1,
  });
  let currentStreamId = narrowSolo.control.streamId;

  await navigateToSmokeSession(widePage, sessionId);
  await waitForPainted(widePage, sessionId, PTY_FIXTURE_READY);
  const firstCombined = await waitForTransition(pages, sessionId, {
    activeIndices: [0, 1],
    activeViewCount: 2,
    previousStreamId: currentStreamId,
  });
  expect(firstCombined.control.streamId).not.toBe(currentStreamId);
  currentStreamId = firstCombined.control.streamId;

  await forceHidden(narrowPage, true);
  const wideSolo = await waitForTransition(pages, sessionId, {
    activeIndices: [0],
    activeViewCount: 1,
    previousStreamId: currentStreamId,
    replicaStreamIds: [undefined, currentStreamId],
    domMayLag: [1],
  });
  expect(wideSolo.control.streamId).not.toBe(currentStreamId);
  currentStreamId = wideSolo.control.streamId;
  expect(wideSolo.control.cols).toBeGreaterThan(narrowSolo.control.cols);
  expect(wideSolo.control.rows).toBeLessThan(narrowSolo.control.rows);
  const crossedMinimum = expectedMinimum(wideSolo.control, narrowSolo.control);
  expect(firstCombined.control).toMatchObject(crossedMinimum);
  expect(firstCombined.control.cols).toBe(narrowSolo.control.cols);
  expect(firstCombined.control.rows).toBe(wideSolo.control.rows);
  expect(wideSolo.control.cols).toBeGreaterThan(firstCombined.control.cols);
  expect(wideSolo.control.rows).toBe(firstCombined.control.rows);

  // Build known normal-screen history, then express real passive-reader intent
  // with a wheel gesture. Geometry reframes must advance canonical state
  // off-DOM while the anchor and already-painted history remain fixed.
  const historyPrefix = `MV-HISTORY-${suffix}-`;
  const historyCount = 2_300;
  await inputSmokeTerminal(widePage, sessionId, encodePtyFixtureCommand({
    op: "FLOOD",
    prefix: historyPrefix,
    count: historyCount,
  }));
  await waitForPainted(widePage, sessionId, `${historyPrefix}${historyCount}`);
  const wideSlot = widePage.getByTestId(`terminal-slot-${sessionId}`);
  const wideBox = await wideSlot.boundingBox();
  if (!wideBox) throw new Error("wide terminal had no painted box for reader input");
  await widePage.mouse.move(wideBox.x + wideBox.width / 2, wideBox.y + wideBox.height / 2);
  await widePage.mouse.wheel(0, -2_400);
  const historyAnchor = await waitForHistoryAnchor(widePage, sessionId, historyPrefix);
  const readerBeforeJoin = await readTerminalStreamProbe(widePage, sessionId);

  await forceHidden(narrowPage, false);
  const joinedHistory = await waitForTransition(pages, sessionId, {
    activeIndices: [0, 1],
    activeViewCount: 2,
    previousStreamId: currentStreamId,
    geometry: crossedMinimum,
    domMayLag: [0],
  });
  expect(joinedHistory.control.streamId).not.toBe(currentStreamId);
  currentStreamId = joinedHistory.control.streamId;
  expectReaderReframeHeld(readerBeforeJoin, joinedHistory.probes[0]!, crossedMinimum);
  await expectHistoryAnchorPreserved(widePage, sessionId, historyAnchor);
  await waitForPainted(narrowPage, sessionId, `${historyPrefix}${historyCount}`);

  await forceHidden(narrowPage, true);
  const releasedHistory = await waitForTransition(pages, sessionId, {
    activeIndices: [0],
    activeViewCount: 1,
    previousStreamId: currentStreamId,
    replicaStreamIds: [undefined, currentStreamId],
    geometry: { cols: wideSolo.control.cols, rows: wideSolo.control.rows },
    domMayLag: [0, 1],
  });
  expect(releasedHistory.control.streamId).not.toBe(currentStreamId);
  currentStreamId = releasedHistory.control.streamId;
  expectReaderReframeHeld(
    joinedHistory.probes[0]!,
    releasedHistory.probes[0]!,
    { cols: wideSolo.control.cols, rows: wideSolo.control.rows },
  );
  await expectHistoryAnchorPreserved(widePage, sessionId, historyAnchor);

  await forceHidden(narrowPage, false);
  const rejoinedHistory = await waitForTransition(pages, sessionId, {
    activeIndices: [0, 1],
    activeViewCount: 2,
    previousStreamId: currentStreamId,
    geometry: crossedMinimum,
    domMayLag: [0],
  });
  expect(rejoinedHistory.control.streamId).not.toBe(currentStreamId);
  currentStreamId = rejoinedHistory.control.streamId;
  expectReaderReframeHeld(releasedHistory.probes[0]!, rejoinedHistory.probes[0]!, crossedMinimum);
  await expectHistoryAnchorPreserved(widePage, sessionId, historyAnchor);
  await widePage.mouse.wheel(0, 100_000);
  await expect.poll(async () => {
    const presentation = (await readTerminalStreamProbe(widePage, sessionId)).browser.presentation;
    return { intent: presentation?.reader_intent ?? null, atBottom: presentation?.at_bottom ?? false };
  }, { timeout: 30_000, intervals: [50, 100, 250] }).toEqual({ intent: "live", atBottom: true });
  let liveCombined = await waitForTransition(pages, sessionId, {
    activeIndices: [0, 1],
    activeViewCount: 2,
    streamId: currentStreamId,
    geometry: crossedMinimum,
  });
  const wideCurrent = liveCombined.probes[0]!.browser;
  expect(wideCurrent.handler_canonical).toEqual(wideCurrent.dom_reconciled);
  expect(wideCurrent.presentation?.canonical).toEqual(wideCurrent.handler_canonical);
  expect(wideCurrent.presentation?.reconciled).toEqual(wideCurrent.handler_canonical);

  // A reframe never makes paging a correctness precondition. Cross the current
  // epoch's actual unpainted boundary before requiring the demand-only RPC.
  const backfillsBeforeDemand = await readBackfillRequestCount(widePage, sessionId);
  const paintedBeforeDemand = await readPaintedScrollback(widePage, sessionId);
  expect(paintedBeforeDemand.headSpacerPx).toBeGreaterThan(0);
  await widePage.evaluate((id) => {
    const container = document.querySelector(
      `[data-testid="terminal-slot-${id}"] .wterm.cell-grid`,
    );
    if (!(container instanceof HTMLElement)) throw new Error("terminal cell grid missing");
    container.scrollTop = 0;
    container.dispatchEvent(new Event("scroll"));
  }, sessionId);
  await expect.poll(
    () => readBackfillRequestCount(widePage, sessionId),
    { timeout: 30_000, intervals: [50, 100, 250] },
  ).toBeGreaterThan(backfillsBeforeDemand);
  await widePage.mouse.wheel(0, 100_000);
  await expect.poll(async () => {
    const presentation = (await readTerminalStreamProbe(widePage, sessionId)).browser.presentation;
    return { intent: presentation?.reader_intent ?? null, atBottom: presentation?.at_bottom ?? false };
  }, { timeout: 30_000, intervals: [50, 100, 250] }).toEqual({ intent: "live", atBottom: true });

  // Paint htop-shaped static chrome once. Every later alternate-screen write
  // before the concurrent-input phase changes numeric cells only.
  expect(liveCombined.control.rows).toBeGreaterThan(SECOND_INPUT_ROW + 2);
  expect(liveCombined.control.cols).toBeGreaterThan(ACTIVITY_COL + 16);
  const altNonce = `MV-ALT-${suffix}`;
  await inputSmokeTerminal(widePage, sessionId, encodePtyFixtureCommand({
    op: "ALT_SCREEN",
    active: true,
    prefix: `MV-ALT-PRIME-${suffix}-`,
    count: 1,
    nonce: altNonce,
  }));
  await Promise.all(pages.map((page) => waitForPainted(page, sessionId, `ALT_READY:${altNonce}`)));
  const htop = htopFrame(suffix);
  await inputSmokeTerminal(widePage, sessionId, htop.payload);
  await Promise.all(pages.map((page) => waitForPainted(page, sessionId, htop.markers.at(-1)!)));
  await Promise.all(pages.map((page) => expectMarkersOnce(page, sessionId, htop.markers)));
  liveCombined = await waitForTransition(pages, sessionId, {
    activeIndices: [0, 1],
    activeViewCount: 2,
    streamId: currentStreamId,
    geometry: crossedMinimum,
  });

  await applyNumericUpdate(widePage, pages, sessionId, htop.markers, "1969", "42.1", "4");
  const beforeWideResize = liveCombined.control;
  await widePage.setViewportSize(RESIZED_WIDE_VIEWPORT);
  await widePage.evaluate(() => window.dispatchEvent(new Event("resize")));
  const afterWideResize = await waitForTransition(pages, sessionId, {
    activeIndices: [0, 1],
    activeViewCount: 2,
    previousStreamId: currentStreamId,
    geometryPredicate: (geometry) => geometry.cols === beforeWideResize.cols
      && geometry.rows > beforeWideResize.rows,
  });
  expect(afterWideResize.control.streamId).not.toBe(currentStreamId);
  currentStreamId = afterWideResize.control.streamId;
  await Promise.all(pages.map((page) => expectMarkersOnce(page, sessionId, htop.markers)));
  await applyNumericUpdate(narrowPage, pages, sessionId, htop.markers, "2077", "57.3", "5");

  const beforeNarrowResize = afterWideResize.control;
  await narrowPage.setViewportSize(RESIZED_NARROW_VIEWPORT);
  await narrowPage.evaluate(() => window.dispatchEvent(new Event("resize")));
  const afterNarrowResize = await waitForTransition(pages, sessionId, {
    activeIndices: [0, 1],
    activeViewCount: 2,
    previousStreamId: currentStreamId,
    geometryPredicate: (geometry) => geometry.cols > beforeNarrowResize.cols
      && geometry.rows === beforeNarrowResize.rows,
  });
  expect(afterNarrowResize.control.streamId).not.toBe(currentStreamId);
  currentStreamId = afterNarrowResize.control.streamId;
  await Promise.all(pages.map((page) => expectMarkersOnce(page, sessionId, htop.markers)));
  await applyNumericUpdate(widePage, pages, sessionId, htop.markers, "3184", "68.2", "6");

  // A real /file surface switch withdraws only the narrow browser. The
  // coordinator releases the SCD to the surviving wide geometry while the
  // hidden browser keeps its replica and DOM watermarks, then rejoins without
  // a renderer remount. Each effective-geometry change receives a fresh stream.
  await narrowPage.evaluate(({ workerFp, token }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.navigate(`/file/${workerFp}/tmp/roost-multiview-${token}.txt`);
  }, { workerFp: fixtureWorker.workerFp, token: suffix });
  await expect.poll(() => narrowPage.evaluate(() => {
    const deck = document.querySelector('[data-testid="terminal-deck"]');
    return deck?.parentElement ? getComputedStyle(deck.parentElement).visibility : null;
  })).toBe("hidden");
  const fileSurface = await waitForTransition(pages, sessionId, {
    activeIndices: [0],
    activeViewCount: 1,
    previousStreamId: currentStreamId,
    replicaStreamIds: [undefined, currentStreamId],
    geometryPredicate: (geometry) => geometry.cols > afterNarrowResize.control.cols
      && geometry.rows === afterNarrowResize.control.rows,
    domMayLag: [1],
  });
  expect(fileSurface.control.streamId).not.toBe(currentStreamId);
  currentStreamId = fileSurface.control.streamId;
  await inputSmokeTerminal(widePage, sessionId, numericUpdate("4295", "73.4", "7"));
  await waitForPainted(widePage, sessionId, "4295");
  await expectMarkersOnce(widePage, sessionId, htop.markers);

  await narrowPage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.navigate(`/s/${id}`);
  }, sessionId);
  await expect(narrowPage.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
  liveCombined = await waitForTransition(pages, sessionId, {
    activeIndices: [0, 1],
    activeViewCount: 2,
    previousStreamId: currentStreamId,
    geometry: { cols: afterNarrowResize.control.cols, rows: afterNarrowResize.control.rows },
  });
  expect(liveCombined.control.streamId).not.toBe(currentStreamId);
  currentStreamId = liveCombined.control.streamId;
  await Promise.all(pages.map((page) => waitForPainted(page, sessionId, "4295")));
  await Promise.all(pages.map((page) => expectMarkersOnce(page, sessionId, htop.markers)));

  // Each context submits one complete fixture frame concurrently. Fixed,
  // disjoint screen rows make admission order irrelevant while any byte-level
  // interleaving or truncation prevents one of the exact markers from painting.
  const firstInputMarker = `INPUT-A-${suffix}`;
  const secondInputMarker = `INPUT-B-${suffix}`;
  const firstInput = encodePtyFixtureCommand({
    op: "EMIT",
    newline: false,
    text: `\x1b[${FIRST_INPUT_ROW};1H${firstInputMarker}`,
  });
  const secondInput = encodePtyFixtureCommand({
    op: "EMIT",
    newline: false,
    text: `\x1b[${SECOND_INPUT_ROW};1H${secondInputMarker}`,
  });
  await Promise.all([
    inputSmokeTerminal(widePage, sessionId, firstInput),
    inputSmokeTerminal(narrowPage, sessionId, secondInput),
  ]);
  await Promise.all(pages.flatMap((page) => [
    waitForPainted(page, sessionId, firstInputMarker),
    waitForPainted(page, sessionId, secondInputMarker),
  ]));
  await Promise.all(pages.map((page) => expectMarkersOnce(
    page,
    sessionId,
    [...htop.markers, firstInputMarker, secondInputMarker],
  )));
  liveCombined = await waitForTransition(pages, sessionId, {
    activeIndices: [0, 1],
    activeViewCount: 2,
    streamId: currentStreamId,
    geometry: { cols: afterNarrowResize.control.cols, rows: afterNarrowResize.control.rows },
  });

  // Pause one context's Sync transport after installing an in-document canary.
  // Its coordinator view is released, the surviving browser continues, and the
  // paused DOM cannot see the numeric update. Resume must first restore a ready
  // baseline, then accept a later differential frame without reloading.
  const streamBeforePause = currentStreamId;
  const beforePause = await narrowPage.evaluate(() => {
    const smokeWindow = window as unknown as Window & {
      __smoke: RecoverySmokeApi;
      __terminalMultiviewCanary?: object;
    };
    smokeWindow.__terminalMultiviewCanary = { alive: true };
    const generation = smokeWindow.__smoke.syncWsGeneration();
    smokeWindow.__smoke.pauseSyncTransport();
    return { generation, href: location.href };
  });
  const narrowEpochBeforePause = nonEmptyString(
    liveCombined.probes[1]!.browser.replica.grid_epoch,
    "narrow replica epoch before transport pause",
  );
  const paused = await waitForTransition(pages, sessionId, {
    activeIndices: [0],
    activeViewCount: 1,
    previousStreamId: currentStreamId,
    desiredActiveIndices: [0, 1],
    replicaStreamIds: [undefined, streamBeforePause],
    geometry: { cols: fileSurface.control.cols, rows: fileSurface.control.rows },
    domMayLag: [1],
  });
  expect(paused.control.streamId).not.toBe(currentStreamId);
  currentStreamId = paused.control.streamId;

  await inputSmokeTerminal(widePage, sessionId, numericUpdate("5306", "81.5", "8"));
  await waitForPainted(widePage, sessionId, "5306");
  const pausedText = await viewportText(narrowPage, sessionId);
  expect(pausedText).toContain("4295");
  expect(pausedText).not.toContain("5306");
  await narrowPage.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.resumeSyncTransport();
  });
  const resumed = await waitForTransition(pages, sessionId, {
    activeIndices: [0, 1],
    activeViewCount: 2,
    previousStreamId: currentStreamId,
    geometry: { cols: afterNarrowResize.control.cols, rows: afterNarrowResize.control.rows },
  });
  expect(resumed.control.streamId).not.toBe(currentStreamId);
  currentStreamId = resumed.control.streamId;
  await waitForPainted(narrowPage, sessionId, "5306");
  expect(resumed.probes[1]!.browser.replica.grid_epoch).not.toBe(narrowEpochBeforePause);
  const afterResume = await narrowPage.evaluate(() => {
    const smokeWindow = window as unknown as Window & {
      __smoke: RecoverySmokeApi;
      __terminalMultiviewCanary?: object;
    };
    return {
      sameDocument: smokeWindow.__terminalMultiviewCanary !== undefined,
      generation: smokeWindow.__smoke.syncWsGeneration(),
      href: location.href,
    };
  });
  expect(afterResume).toEqual({
    sameDocument: true,
    generation: expect.any(Number),
    href: beforePause.href,
  });
  expect(afterResume.generation).toBeGreaterThan(beforePause.generation);

  const postBaselineFloors = resumed.probes.map((probe, index) =>
    integer(probe.browser.replica.seq, `browser ${index} post-resume baseline sequence`));
  await applyNumericUpdate(narrowPage, pages, sessionId, htop.markers, "6417", "92.6", "9");
  const postBaselineDelta = await waitForTransition(pages, sessionId, {
    activeIndices: [0, 1],
    activeViewCount: 2,
    streamId: currentStreamId,
    geometry: { cols: afterNarrowResize.control.cols, rows: afterNarrowResize.control.rows },
  });
  for (const [index, probe] of postBaselineDelta.probes.entries()) {
    expect(integer(probe.browser.replica.seq, `browser ${index} post-baseline delta sequence`))
      .toBeGreaterThan(postBaselineFloors[index]!);
  }
  await Promise.all(pages.map((page) => expectMarkersOnce(
    page,
    sessionId,
    [...htop.markers, firstInputMarker, secondInputMarker],
  )));
});
