// Mobile terminal right-edge proofs reject late-font geometry and text inflation.
// Worker grid dimensions own columns while .cell-grid owns fixed cell geometry.
// Real shells and DOM Ranges check final glyphs against terminal and visual clips.
// A wide font exercises lifecycle invalidation; parent inflation exercises CSS isolation.
// No proof infers visibility from row text or adds terminal geometry math.

import type { Page } from "@playwright/test";
import { test, expect } from "./fixtures.ts";
import {
  navigateToSmokeSession,
  spawnSmokeShell,
  waitForStableCellFrames,
} from "./terminal-helpers.ts";
import { forceHidden, forceVisible } from "./terminal-multiview-helpers.ts";
import {
  expectSheetFitsPane,
  measureCellAdvance,
  paintAndProveLastColumn,
  readDimensions,
  SETTLE_INTERVALS,
  SETTLE_TIMEOUT_MS,
  waitForConvergedColumns,
} from "./terminal-mobile-width-proof.ts";

const REGRESSION_FONT_FAMILY = "RoostWidthRegression";
const REGRESSION_FONT_PATH = "/fonts/JetBrainsMonoNerdFontMono-Regular.woff2";
// A query the production sheet never requests: uniquely routable, and never
// answered from the already-cached production face.
const REGRESSION_FONT_QUERY = "mobile-width-regression";
const DESKTOP_VIEWPORT = { width: 1_280, height: 860 };
const NARROW_VIEWPORT = { width: 360, height: 780 };
// size-adjust: 125% against measurement noise. Below this the swap never
// landed and every geometry claim behind it would be vacuous.
const LOADED_ADVANCE_RATIO = 1.2;

const TEXT_INFLATION_CONTROL_TEST_ID = "terminal-text-inflation-control";
const TEXT_INFLATION_ROOT_ATTRIBUTE = "data-terminal-text-inflation-root";
const TEXT_INFLATION_STYLE_TEST_ID = "terminal-text-inflation-style";
const TEXT_INFLATION_UNPROTECTED_STYLE_TEST_ID = "terminal-text-inflation-unprotected-style";
const TEXT_INFLATION_ASCII = "0123456789";

type TerminalGridBox = {
  left: number;
  top: number;
  width: number;
  height: number;
  clientWidth: number;
  clientHeight: number;
};

type TextInflationProbe = {
  controlWidth: number;
  terminalBox: TerminalGridBox;
};

async function installTextInflationControl(page: Page, sessionId: string): Promise<void> {
  await page.evaluate(({ id, controlTestId, text }) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    if (!(slot instanceof HTMLElement)) throw new Error(`terminal slot unavailable for ${id}`);
    if (slot.querySelector(`[data-testid="${controlTestId}"]`)) {
      throw new Error(`text inflation control already installed for ${id}`);
    }
    const control = document.createElement("span");
    control.dataset.testid = controlTestId;
    control.setAttribute("aria-hidden", "true");
    control.textContent = text;
    control.style.cssText = "position:absolute;top:0;left:0;opacity:0;pointer-events:none;"
      + "white-space:pre;font:16px/1 monospace;";
    slot.append(control);
  }, {
    id: sessionId,
    controlTestId: TEXT_INFLATION_CONTROL_TEST_ID,
    text: TEXT_INFLATION_ASCII,
  });
}

async function readTextInflationProbe(page: Page, sessionId: string): Promise<TextInflationProbe> {
  return page.evaluate(({ id, controlTestId }) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const grid = slot?.querySelector(".cell-grid");
    const control = slot?.querySelector(`[data-testid="${controlTestId}"]`);
    if (!(grid instanceof HTMLElement) || !(control instanceof HTMLElement)) {
      throw new Error(`text inflation probe unavailable for ${id}`);
    }
    const range = document.createRange();
    range.selectNodeContents(control);
    const rect = grid.getBoundingClientRect();
    return {
      controlWidth: range.getBoundingClientRect().width,
      terminalBox: {
        left: rect.left,
        top: rect.top,
        width: rect.width,
        height: rect.height,
        clientWidth: grid.clientWidth,
        clientHeight: grid.clientHeight,
      },
    };
  }, { id: sessionId, controlTestId: TEXT_INFLATION_CONTROL_TEST_ID });
}

async function enableTextInflationCalibration(page: Page, sessionId: string): Promise<void> {
  await page.evaluate(({ id, rootAttribute, styleTestId }) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    if (!(slot instanceof HTMLElement)) throw new Error(`terminal slot unavailable for ${id}`);
    if (document.querySelector(`[data-testid="${styleTestId}"]`)) {
      throw new Error(`text inflation calibration already enabled for ${id}`);
    }
    const style = document.createElement("style");
    style.dataset.testid = styleTestId;
    style.textContent = `[${rootAttribute}]{-webkit-text-size-adjust:200%;text-size-adjust:200%;}`;
    slot.setAttribute(rootAttribute, "true");
    document.head.append(style);
  }, {
    id: sessionId,
    rootAttribute: TEXT_INFLATION_ROOT_ATTRIBUTE,
    styleTestId: TEXT_INFLATION_STYLE_TEST_ID,
  });
}

async function setTerminalInflationProtectionDisabled(
  page: Page,
  sessionId: string,
  disabled: boolean,
): Promise<void> {
  await page.evaluate(({ id, styleTestId, shouldDisable }) => {
    document.querySelector(`[data-testid="${styleTestId}"]`)?.remove();
    if (!shouldDisable) return;
    const style = document.createElement("style");
    style.dataset.testid = styleTestId;
    style.textContent = `[data-testid="terminal-slot-${id}"] .cell-grid{`
      + "-webkit-text-size-adjust:inherit!important;text-size-adjust:inherit!important;}";
    document.head.append(style);
  }, {
    id: sessionId,
    styleTestId: TEXT_INFLATION_UNPROTECTED_STYLE_TEST_ID,
    shouldDisable: disabled,
  });
}

async function removeTextInflationProbe(page: Page, sessionId: string): Promise<void> {
  await page.evaluate(({ id, controlTestId, rootAttribute, styleTestId, unprotectedStyleTestId }) => {
    document.querySelector(`[data-testid="${styleTestId}"]`)?.remove();
    document.querySelector(`[data-testid="${unprotectedStyleTestId}"]`)?.remove();
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    slot?.removeAttribute(rootAttribute);
    slot?.querySelector(`[data-testid="${controlTestId}"]`)?.remove();
  }, {
    id: sessionId,
    controlTestId: TEXT_INFLATION_CONTROL_TEST_ID,
    rootAttribute: TEXT_INFLATION_ROOT_ATTRIBUTE,
    styleTestId: TEXT_INFLATION_STYLE_TEST_ID,
    unprotectedStyleTestId: TEXT_INFLATION_UNPROTECTED_STYLE_TEST_ID,
  });
}


test("a terminal font that loads while hidden keeps the mobile right edge reachable", async ({
  mobileSmokePage,
  stack,
}, testInfo) => {
  test.setTimeout(180_000);
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);
  await mobileSmokePage.evaluate(async () => { await document.fonts.ready; });
  await waitForStableCellFrames(mobileSmokePage, sessionId);
  await expectSheetFitsPane(mobileSmokePage, sessionId, "settled production fonts");
  const productionAdvance = await measureCellAdvance(mobileSmokePage, sessionId);
  expect(productionAdvance, "the production face measured a cell advance").toBeGreaterThan(0);

  const releaseFont = Promise.withResolvers<void>();
  const fontRouteFinished = Promise.withResolvers<void>();
  let fontRequested = false;
  const regressionFontRequest = (url: URL): boolean =>
    url.pathname.endsWith(REGRESSION_FONT_PATH) && url.search === `?${REGRESSION_FONT_QUERY}`;
  await mobileSmokePage.route(regressionFontRequest, async (route) => {
    fontRequested = true;
    try {
      await releaseFont.promise;
      await route.continue();
    } finally {
      fontRouteFinished.resolve();
    }
  });
  try {
    await mobileSmokePage.evaluate(({ id, family, src }) => {
      const style = document.createElement("style");
      style.textContent = `@font-face{font-family:'${family}';src:url('${src}') format('woff2');`
        + "font-display:swap;size-adjust:125%;}"
        + `[data-testid="terminal-slot-${id}"] .cell-grid`
        + `{--term-font-family:'${family}','JBM Nerd',monospace;}`;
      document.head.append(style);
    }, {
      id: sessionId,
      family: REGRESSION_FONT_FAMILY,
      src: `${REGRESSION_FONT_PATH}?${REGRESSION_FONT_QUERY}`,
    });
    await expect.poll(() => fontRequested, {
      timeout: SETTLE_TIMEOUT_MS,
      intervals: SETTLE_INTERVALS,
      message: "the pane requested the regression face",
    }).toBe(true);

    // Hide the SPA before releasing the face: the swap then completes in a
    // loading epoch this pane can observe but must not publish from.
    const hiddenCols = (await readDimensions(mobileSmokePage, sessionId)).cols;
    await forceHidden(mobileSmokePage, true);
    releaseFont.resolve();
    await expect.poll(() => mobileSmokePage.evaluate((family) => {
      const statuses: string[] = [];
      document.fonts.forEach((face) => {
        if (face.family.includes(family)) statuses.push(face.status);
      });
      return statuses.join(",") || "absent";
    }, REGRESSION_FONT_FAMILY), {
      timeout: SETTLE_TIMEOUT_MS,
      intervals: SETTLE_INTERVALS,
      message: "the regression face finished loading",
    }).toBe("loaded");
    await mobileSmokePage.evaluate(async () => { await document.fonts.ready; });
    expect(
      (await readDimensions(mobileSmokePage, sessionId)).cols,
      "a hidden pane claims no geometry while its font settles",
    ).toBe(hiddenCols);

    await forceVisible(mobileSmokePage, true);
    const loadedAdvance = await measureCellAdvance(mobileSmokePage, sessionId);
    expect(
      loadedAdvance / productionAdvance,
      `the 125% face widened the advance: ${productionAdvance} → ${loadedAdvance}`,
    ).toBeGreaterThanOrEqual(LOADED_ADVANCE_RATIO);
    await paintAndProveLastColumn(
      [mobileSmokePage],
      mobileSmokePage,
      sessionId,
      "revealed after the font swap",
    );

    const portrait = mobileSmokePage.viewportSize();
    if (!portrait) throw new Error("the mobile page reported no viewport size");
    await mobileSmokePage.setViewportSize({ width: portrait.height, height: portrait.width });
    await paintAndProveLastColumn([mobileSmokePage], mobileSmokePage, sessionId, "landscape");
    await mobileSmokePage.setViewportSize(portrait);
    await paintAndProveLastColumn([mobileSmokePage], mobileSmokePage, sessionId, "restored portrait");
    await testInfo.attach("mobile-terminal-right-edge.png", {
      body: await mobileSmokePage.screenshot(),
      contentType: "image/png",
    });
  } finally {
    releaseFont.resolve();
    if (fontRequested) await fontRouteFinished.promise.catch(() => undefined);
    await mobileSmokePage.unroute(regressionFontRequest).catch(() => undefined);
    await forceVisible(mobileSmokePage, false).catch(() => undefined);
  }
});

test.describe("WebKit iPhone text inflation", () => {
  test.skip(({ browserName }) => browserName !== "webkit", "iPhone text-inflation contract");

  test("mobile text inflation cannot enlarge a settled terminal grid", async ({
    mobileSmokePage,
    stack,
  }) => {
  test.setTimeout(180_000);
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);
  await mobileSmokePage.evaluate(async () => { await document.fonts.ready; });
  await waitForStableCellFrames(mobileSmokePage, sessionId);
  await expectSheetFitsPane(mobileSmokePage, sessionId, "settled production fonts before inflation");
  await installTextInflationControl(mobileSmokePage, sessionId);
  try {
    const baselineDimensions = await readDimensions(mobileSmokePage, sessionId);
    const baselineAdvance = await measureCellAdvance(mobileSmokePage, sessionId);
    const baselineProbe = await readTextInflationProbe(mobileSmokePage, sessionId);
    expect(baselineDimensions.cols, "the settled grid reported columns").toBeGreaterThan(0);
    expect(baselineAdvance, "the settled grid measured a cell advance").toBeGreaterThan(0);
    expect(baselineProbe.controlWidth, "the external ASCII control measured a DOM Range").toBeGreaterThan(0);

    await enableTextInflationCalibration(mobileSmokePage, sessionId);
    await expect.poll(
      async () => (await readTextInflationProbe(mobileSmokePage, sessionId)).controlWidth,
      {
        timeout: SETTLE_TIMEOUT_MS,
        intervals: SETTLE_INTERVALS,
        message: "the WebKit text-inflation calibration enlarged its external ASCII control",
      },
    ).toBeGreaterThanOrEqual(baselineProbe.controlWidth * 1.5);

    await setTerminalInflationProtectionDisabled(mobileSmokePage, sessionId, true);
    await expect.poll(
      () => measureCellAdvance(mobileSmokePage, sessionId),
      {
        timeout: SETTLE_TIMEOUT_MS,
        intervals: SETTLE_INTERVALS,
        message: "the calibrated grid advance grows when its inflation protection is disabled",
      },
    ).toBeGreaterThanOrEqual(baselineAdvance * 1.5);
    expect(
      (await readDimensions(mobileSmokePage, sessionId)).cols,
      "text inflation changes glyph fit without changing the worker grid",
    ).toBe(baselineDimensions.cols);
    await setTerminalInflationProtectionDisabled(mobileSmokePage, sessionId, false);
    await expect.poll(
      () => measureCellAdvance(mobileSmokePage, sessionId),
      {
        timeout: SETTLE_TIMEOUT_MS,
        intervals: SETTLE_INTERVALS,
        message: "restoring the grid protection restores its measured cell advance",
      },
    ).toBe(baselineAdvance);

    const inflatedDimensions = await readDimensions(mobileSmokePage, sessionId);
    const inflatedAdvance = await measureCellAdvance(mobileSmokePage, sessionId);
    const inflatedProbe = await readTextInflationProbe(mobileSmokePage, sessionId);
    expect(inflatedDimensions.cols, "text inflation leaves settled terminal columns unchanged")
      .toBe(baselineDimensions.cols);
    expect(inflatedAdvance, "text inflation leaves the ten-cell advance unchanged")
      .toBe(baselineAdvance);
    expect(inflatedProbe.terminalBox, "text inflation leaves the terminal box unchanged")
      .toEqual(baselineProbe.terminalBox);

    await paintAndProveLastColumn(
      [mobileSmokePage],
      mobileSmokePage,
      sessionId,
      "inflated external text",
    );
    const portrait = mobileSmokePage.viewportSize();
    if (!portrait) throw new Error("the mobile page reported no viewport size");
    await mobileSmokePage.setViewportSize({ width: portrait.height, height: portrait.width });
    await paintAndProveLastColumn(
      [mobileSmokePage],
      mobileSmokePage,
      sessionId,
      "inflated external text in landscape",
    );
    await mobileSmokePage.setViewportSize(portrait);
    await paintAndProveLastColumn(
      [mobileSmokePage],
      mobileSmokePage,
      sessionId,
      "inflated external text in restored portrait",
    );
  } finally {
    await removeTextInflationProbe(mobileSmokePage, sessionId).catch(() => undefined);
  }
  });
});

test("a revealed narrow viewer converges one shell back onto a visible last column", async ({
  smokePage,
  secondSmokePage,
  stack,
}) => {
  test.setTimeout(180_000);
  await Promise.all([
    smokePage.setViewportSize(DESKTOP_VIEWPORT),
    secondSmokePage.setViewportSize(NARROW_VIEWPORT),
    forceVisible(smokePage, true),
    forceVisible(secondSmokePage, true),
  ]);
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await navigateToSmokeSession(secondSmokePage, sessionId);
  await Promise.all([
    waitForStableCellFrames(smokePage, sessionId),
    waitForStableCellFrames(secondSmokePage, sessionId),
  ]);
  const sharedCols = await waitForConvergedColumns(
    [smokePage, secondSmokePage],
    sessionId,
    "both viewers joined",
  );

  await forceHidden(secondSmokePage, true);
  await expect.poll(async () => (await readDimensions(smokePage, sessionId)).cols, {
    timeout: SETTLE_TIMEOUT_MS,
    intervals: SETTLE_INTERVALS,
    message: "the parked narrow viewer stopped constraining the shared grid",
  }).toBeGreaterThan(sharedCols);

  await forceVisible(secondSmokePage, true);
  const convergedCols = await paintAndProveLastColumn(
    [smokePage, secondSmokePage],
    smokePage,
    sessionId,
    "narrow viewer rejoined",
  );
  expect(convergedCols, "the rejoined viewer re-imposes its own grid on the session").toBe(sharedCols);
});
