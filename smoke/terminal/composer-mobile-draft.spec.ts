import { test, expect } from "./fixtures.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import { spawnSmokeShell, navigateToSmokeSession, inputSmokeTerminal } from "./terminal-helpers.ts";
import {
  acceptedGeometry,
  coordinatorTerminalViewState,
  readTerminalStreamProbe,
} from "./terminal-probe-helpers.ts";

// Compact composition is structurally two rows from its first paint. The field
// always owns the full first row; its content never decides whether the actions
// consume message width. Explicit lines and ordinary wrapping therefore share
// one stable wrap width while only the top of the fixed dock moves.
test("mobile composer gives multiline drafts the full message width", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const box = mobileSmokePage.getByTestId("chat-box");
  const input = mobileSmokePage.getByTestId("chat-input");

  const readComposerGeometry = () => mobileSmokePage.evaluate((id) => {
    const dockEl = document.querySelector('[data-testid="mobile-chat-input"]');
    const boxEl = document.querySelector('[data-testid="chat-box"]');
    const inputEl = document.querySelector('[data-testid="chat-input"]');
    const micEl = document.querySelector('[data-testid="voice-mic"]');
    const sendEl = document.querySelector('[data-testid="chat-send"]');
    const deckEl = document.querySelector('[data-testid="terminal-deck"]');
    const terminalEl = document.querySelector(
      `[data-testid="terminal-slot-${id}"] [data-testid="terminal-display"]`,
    );
    if (
      !(dockEl instanceof HTMLElement)
      || !(boxEl instanceof HTMLElement)
      || !(inputEl instanceof HTMLTextAreaElement)
      || !(micEl instanceof HTMLElement)
      || !(sendEl instanceof HTMLElement)
      || !(deckEl instanceof HTMLElement)
      || !(terminalEl instanceof HTMLElement)
    ) return null;

    const rect = (el: HTMLElement) => {
      const value = el.getBoundingClientRect();
      return {
        x: value.x,
        y: value.y,
        top: value.top,
        right: value.right,
        bottom: value.bottom,
        left: value.left,
        width: value.width,
        height: value.height,
      };
    };
    const px = (value: string) => {
      const parsed = Number.parseFloat(value);
      return Number.isFinite(parsed) ? parsed : 0;
    };
    const resolveMaxHeight = (value: string) => {
      const probe = document.createElement("div");
      probe.style.cssText = [
        "position:fixed",
        "visibility:hidden",
        "pointer-events:none",
        "box-sizing:border-box",
        "height:10000px",
      ].join(";");
      probe.style.maxHeight = value;
      document.body.append(probe);
      const height = probe.getBoundingClientRect().height;
      probe.remove();
      return height;
    };

    const boxRect = rect(boxEl);
    const boxStyle = getComputedStyle(boxEl);
    const inputStyle = getComputedStyle(inputEl);
    const paddingLeft = px(boxStyle.paddingLeft);
    const paddingRight = px(boxStyle.paddingRight);
    return {
      dock: rect(dockEl),
      box: boxRect,
      input: rect(inputEl),
      mic: rect(micEl),
      send: rect(sendEl),
      deck: rect(deckEl),
      deckOffsetHeight: deckEl.offsetHeight,
      deckTransform: getComputedStyle(deckEl).transform,
      terminal: rect(terminalEl),
      terminalClientHeight: terminalEl.clientHeight,
      innerLeft: boxRect.left + paddingLeft,
      innerRight: boxRect.right - paddingRight,
      paddingTop: px(boxStyle.paddingTop),
      paddingBottom: px(boxStyle.paddingBottom),
      rowGap: px(boxStyle.rowGap),
      columnGap: px(boxStyle.columnGap),
      inputClientHeight: inputEl.clientHeight,
      inputScrollHeight: inputEl.scrollHeight,
      lineHeight: px(inputStyle.lineHeight),
      overflowY: inputStyle.overflowY,
      maxHeight: resolveMaxHeight(inputStyle.maxHeight),
      contractMaxHeight: resolveMaxHeight("min(192px, 30dvh)"),
    };
  }, sessionId);

  const geometry = async () => {
    const value = await readComposerGeometry();
    expect(value, "compact composer geometry").not.toBeNull();
    return value!;
  };
  const near = (actual: number, expected: number, tolerance: number, label: string) =>
    expect(Math.abs(actual - expected), label).toBeLessThanOrEqual(tolerance);

  // The entrance animation translates the pill. Finish it before recording the
  // fixed footer coordinates used throughout the growth assertions.
  await box.evaluate(async (el) => {
    await Promise.all(el.getAnimations().map((animation) => animation.finished));
  });
  await input.fill("short");
  const baseline = await geometry();
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(mobileSmokePage, sessionId);
    const { view, replica } = probe.browser;
    const coordinator = coordinatorTerminalViewState(probe);
    const geometry = acceptedGeometry(view);
    return view.status === "accepted"
      && view.active
      && geometry !== null && geometry.cols > 0 && geometry.rows > 0
      && replica.baseline_ready
      && replica.expected_stream_id === view.stream_id
      && coordinator?.activeViews === 1
      && coordinator.streamId === view.stream_id
      && coordinator.effective?.cols === view.effective_cols
      && coordinator.effective?.rows === view.effective_rows;
  }, { timeout: 10_000, intervals: [50] }).toBe(true);
  const baselineProbe = await readTerminalStreamProbe(mobileSmokePage, sessionId);
  const baselineView = baselineProbe.browser.view;
  if (!baselineView.revision || !baselineView.stream_id) {
    throw new Error("compact terminal omitted its active baseline view");
  }

  const expectFullWidthRows = (
    current: typeof baseline,
    label: string,
  ) => {
    const innerWidth = current.innerRight - current.innerLeft;
    near(current.input.left, current.innerLeft, 1, `${label}: textarea left`);
    near(current.input.width, innerWidth, 1, `${label}: textarea uses the box inner width`);
    near(current.input.right, current.innerRight, 1, `${label}: textarea right`);
    expect(
      current.input.right - current.mic.left,
      `${label}: textarea continues across the mic and send columns`,
    ).toBeGreaterThanOrEqual(current.mic.width + current.send.width - 2);
    expect(
      current.input.right - current.send.left,
      `${label}: textarea continues across the send column`,
    ).toBeGreaterThanOrEqual(current.send.width - 2);

    near(current.mic.top, current.send.top, 1, `${label}: action row top`);
    near(current.mic.bottom, current.send.bottom, 1, `${label}: action row bottom`);
    near(current.send.right, current.innerRight, 1, `${label}: footer is right-aligned`);
    near(
      current.send.left - current.mic.right,
      current.columnGap,
      1,
      `${label}: action gap`,
    );
    near(
      current.mic.top - current.input.bottom,
      current.rowGap,
      1,
      `${label}: textarea-to-footer gap`,
    );
    expect(current.mic.top, `${label}: mic is below the textarea`).toBeGreaterThan(
      current.input.bottom,
    );
  };
  const expectStableFooter = (
    current: typeof baseline,
    label: string,
  ) => {
    for (const control of ["mic", "send"] as const) {
      for (const dimension of ["x", "y", "width", "height"] as const) {
        near(
          current[control][dimension],
          baseline[control][dimension],
          1,
          `${label}: stable ${control}.${dimension}`,
        );
      }
    }
    near(current.box.left, baseline.box.left, 1, `${label}: stable box left`);
    near(current.box.width, baseline.box.width, 1, `${label}: stable box width`);
    near(current.input.left, baseline.input.left, 1, `${label}: stable textarea left`);
    near(current.input.width, baseline.input.width, 1, `${label}: stable textarea width`);
  };

  expectFullWidthRows(baseline, "resting");
  near(baseline.input.height, 40, 1, "resting textarea height");
  near(baseline.mic.height, 44, 1, "resting mic height");
  near(baseline.send.height, 44, 1, "resting send height");
  near(baseline.rowGap, 4, 0.5, "resting row gap");
  near(baseline.paddingTop + baseline.paddingBottom, 8, 1, "resting outer padding");
  near(baseline.box.height, 96, 1, "settled compact box height");
  near(baseline.dock.height, 96, 1, "settled compact dock height");

  const explicitLines = [
    "first explicit line",
    "second explicit line",
    "third explicit line",
    "fourth explicit line",
  ].join("\n");
  await input.fill(explicitLines);
  await expect(input).toHaveValue(explicitLines);
  const multiline = await geometry();
  expectFullWidthRows(multiline, "explicit newlines");
  expectStableFooter(multiline, "explicit newlines");
  expect(multiline.input.height, "explicit newlines grow the textarea").toBeGreaterThan(
    baseline.input.height + multiline.lineHeight,
  );
  expect(multiline.input.height, "explicit newlines remain below the cap").toBeLessThan(
    multiline.maxHeight - 1,
  );
  expect(multiline.dock.height, "explicit newlines grow the dock").toBeGreaterThan(
    baseline.dock.height + 1,
  );
  expect(multiline.dock.top, "the growing dock moves upward").toBeLessThan(
    baseline.dock.top - 1,
  );
  near(multiline.dock.bottom, baseline.dock.bottom, 1, "explicit newlines: stable dock bottom");

  // Deliberately abundant prose guarantees overflow without choosing a string
  // near any wrap threshold; text width never selects a different layout.
  const wrappingDraft = (
    "ordinary wrapping keeps every line as wide as the message surface "
  ).repeat(80);
  await input.fill(wrappingDraft);
  await expect(input).toHaveValue(wrappingDraft);
  const capped = await geometry();
  expectFullWidthRows(capped, "long wrapping draft");
  expectStableFooter(capped, "long wrapping draft");
  near(capped.maxHeight, capped.contractMaxHeight, 1, "compact textarea CSS max-height");
  near(capped.input.height, capped.maxHeight, 1, "long draft is capped at CSS max-height");
  expect(
    capped.inputScrollHeight - capped.inputClientHeight,
    "capped textarea has internally scrollable overflow",
  ).toBeGreaterThan(capped.lineHeight);
  expect(capped.overflowY).toBe("auto");
  expect(capped.dock.height, "wrapped text grows the dock to its cap").toBeGreaterThan(
    multiline.dock.height + 1,
  );
  expect(capped.dock.top, "capped dock grows upward").toBeLessThan(multiline.dock.top - 1);
  near(capped.dock.bottom, baseline.dock.bottom, 1, "long wrapping draft: stable dock bottom");

  await expect.poll(async () => {
    const current = await readComposerGeometry();
    return current ? baseline.deck.top - current.deck.top : 0;
  }, {
    message: "compact composer growth must translate the deck without changing its layout box",
  }).toBeGreaterThan(1);
  const transformed = await geometry();
  near(transformed.deckOffsetHeight, baseline.deckOffsetHeight, 1, "compact deck layout height");
  near(transformed.deck.height, baseline.deck.height, 1, "compact deck painted height");
  near(transformed.terminalClientHeight, baseline.terminalClientHeight, 1, "compact terminal layout height");
  near(transformed.terminal.height, baseline.terminal.height, 1, "compact terminal painted height");
  expect(transformed.deck.top, "compact deck paint translates upward").toBeLessThan(baseline.deck.top - 1);
  near(
    baseline.terminal.top - transformed.terminal.top,
    baseline.deck.top - transformed.deck.top,
    1,
    "terminal and deck share the same compact paint transform",
  );
  expect(transformed.deckTransform).not.toBe(baseline.deckTransform);

  // Wait past the desktop ResizeObserver debounce. Compact growth is a paint
  // transform only, so it must not publish a new terminal-view revision while
  // output and the existing replica baseline stay live.
  await mobileSmokePage.waitForTimeout(250);
  const compactProbe = await readTerminalStreamProbe(mobileSmokePage, sessionId);
  const compactView = compactProbe.browser.view;
  expect(compactView).toMatchObject({
    revision: baselineView.revision,
    status: "accepted",
    active: true,
    stream_id: baselineView.stream_id,
    effective_cols: baselineView.effective_cols,
    effective_rows: baselineView.effective_rows,
  });
  expect(compactProbe.browser.replica).toMatchObject({
    expected_stream_id: baselineView.stream_id,
    baseline_ready: true,
    resync_latched: false,
  });
  expect(coordinatorTerminalViewState(compactProbe)).toMatchObject({
    activeViews: 1,
    streamId: baselineView.stream_id,
    effective: {
      cols: baselineView.effective_cols,
      rows: baselineView.effective_rows,
    },
    unavailable: false,
  });
  const compactGrowthHead = "COMPACT_GROWTH_";
  const compactGrowthTail = `LIVE_${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}`;
  const compactGrowthMarker = `${compactGrowthHead}${compactGrowthTail}`;
  // The shell's ordinary bottom-row output may sit outside the visual clip
  // while the fixed composer translates the unchanged terminal layout upward.
  // Pick a fully visible viewport row and paint the marker there; split its
  // shell arguments so the echoed command cannot itself satisfy the proof.
  const compactGrowthRow = await mobileSmokePage.evaluate((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const grid = slot?.querySelector(".cell-grid");
    const viewport = grid?.querySelector(".cell-viewport");
    if (!(grid instanceof HTMLElement) || !(viewport instanceof HTMLElement)) {
      throw new Error("compact terminal viewport was unavailable");
    }
    const gridRect = grid.getBoundingClientRect();
    const visual = window.visualViewport;
    const visualTop = visual?.offsetTop ?? 0;
    const visualBottom = visualTop + (visual?.height ?? innerHeight);
    const clipTop = Math.max(gridRect.top, visualTop);
    const clipBottom = Math.min(gridRect.bottom, visualBottom);
    const clipCenter = (clipTop + clipBottom) / 2;
    const candidates = Array.from(viewport.querySelectorAll(":scope > .cell-row"))
      .flatMap((row, index) => {
        const rect = row.getBoundingClientRect();
        return rect.width > 0
          && rect.height > 0
          && rect.top >= clipTop + 1
          && rect.bottom <= clipBottom - 1
          ? [{ index, distance: Math.abs((rect.top + rect.bottom) / 2 - clipCenter) }]
          : [];
      })
      .sort((left, right) => left.distance - right.distance);
    if (!candidates[0]) {
      throw new Error(`compact terminal has no fully visible output row: ${JSON.stringify({
        gridTop: gridRect.top,
        gridBottom: gridRect.bottom,
        visualTop,
        visualBottom,
      })}`);
    }
    return candidates[0].index + 1;
  }, sessionId);
  await inputSmokeTerminal(
    mobileSmokePage,
    sessionId,
    `printf '\\033[${compactGrowthRow};1H%s%s' '${compactGrowthHead}' '${compactGrowthTail}'\r`,
  );
  const compactGrowthProof = await mobileSmokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: compactGrowthMarker });
  expect(compactGrowthProof).toMatchObject({ marker: compactGrowthMarker, frames: 2 });
  const afterCompactGrowth = await readTerminalStreamProbe(mobileSmokePage, sessionId);
  expect(afterCompactGrowth.browser.handler_canonical).toEqual(afterCompactGrowth.browser.dom_reconciled);
  expect(afterCompactGrowth.browser.presentation?.reader_intent).toBe("live");
  await expect(input).toHaveValue(wrappingDraft);

  await input.fill("short");
  await expect.poll(async () => Math.abs((await geometry()).deck.top - baseline.deck.top))
    .toBeLessThanOrEqual(1);
  const restored = await geometry();
  expectFullWidthRows(restored, "shortened draft");
  for (const surface of ["dock", "box", "input", "mic", "send"] as const) {
    for (const dimension of ["x", "y", "width", "height"] as const) {
      near(
        restored[surface][dimension],
        baseline[surface][dimension],
        1,
        `shortened draft restores ${surface}.${dimension}`,
      );
    }
  }
  for (const surface of ["deck", "terminal"] as const) {
    for (const dimension of ["top", "height"] as const) {
      near(
        restored[surface][dimension],
        baseline[surface][dimension],
        1,
        `shortened draft restores ${surface}.${dimension}`,
      );
    }
  }
});
