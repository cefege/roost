// These callbacks measure terminal reveal, resize, trusted-key, and optimistic-paint paths.
// The perf spec keeps their registrations and known-broken status in one scheduled file.
// Shared fixture operations preserve identical PTY setup and qualification thresholds.

import type { Page, TestInfo } from "@playwright/test";
import type { SmokeApi } from "../../apps/web/src/lib/smoke.ts";
import { test, expect } from "./fixtures.ts";
import { encodePtyFixtureCommand, PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import type { TerminalTestStack } from "./stack.ts";
import { installTerminalLoadingStageProbe, terminalLoadingStages } from "./terminal-loading-stage-probe.ts";
import {
  QUALIFY,
  navigateAndProve,
  percentile,
  spawnFixtureSession,
} from "./perf-probe-fixture.ts";

type SmokeWindow = Window & { readonly __smoke: SmokeApi };
declare const window: SmokeWindow;
export async function probeTerminalInteractions(
  { smokePage, stack }: { smokePage: Page; stack: TerminalTestStack },
  testInfo: TestInfo,
): Promise<void> {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop interaction distributions");
  test.setTimeout(360_000);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  const shallowId = await spawnFixtureSession(smokePage, fixtureWorker);
  const deepId = await spawnFixtureSession(smokePage, fixtureWorker);
  await navigateAndProve(smokePage, shallowId, PTY_FIXTURE_READY);
  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 12);
  const shallowMarker = `SHALLOW_READY:${suffix}`;
  await smokePage.evaluate(async ({ id, frame, marker }) => {
    const smoke = window.__smoke;
    await smoke.input(id, frame);
    await smoke.waitForPaintedMarker(id, marker);
  }, {
    id: shallowId,
    frame: encodePtyFixtureCommand({ op: "EMIT", text: shallowMarker }),
    marker: shallowMarker,
  });

  await navigateAndProve(smokePage, deepId, PTY_FIXTURE_READY);
  const deepMarker = `DEEP_READY:${suffix}`;
  await smokePage.evaluate(async ({ id, floodFrame, finalFrame, floodMarker, marker }) => {
    const smoke = window.__smoke;
    await smoke.input(id, floodFrame);
    await smoke.waitForPaintedMarker(id, floodMarker, 120_000);
    await smoke.input(id, finalFrame);
    await smoke.waitForPaintedMarker(id, marker);
  }, {
    id: deepId,
    floodFrame: encodePtyFixtureCommand({ op: "FLOOD", prefix: "DEEPLINE-", count: 5_000 }),
    finalFrame: encodePtyFixtureCommand({ op: "EMIT", text: deepMarker }),
    floodMarker: "DEEPLINE-5000",
    marker: deepMarker,
  });

  const revealSamples: Array<{ kind: "shallow" | "deep"; ms: number; cached: boolean }> = [];
  for (let sample = 0; sample < 20; sample++) {
    const deep = sample % 2 === 1;
    const id = deep ? deepId : shallowId;
    const marker = deep ? deepMarker : shallowMarker;
    revealSamples.push(await smokePage.evaluate(async ({ id, marker, kind }) => {
      const smoke = window.__smoke;
      const beforeFrames = smoke.cellFrameCount(id);
      const timingId = await smoke.beginTerminalTiming("reveal", id);
      smoke.navigate(`/s/${id}`);
      const result = await smoke.finishTerminalTiming(timingId, id, marker, 30_000);
      return { kind, ms: result.durationMs, cached: smoke.cellFrameCount(id) === beforeFrames };
    }, { id, marker, kind: deep ? "deep" as const : "shallow" as const }));
  }
  const shallowReveal = revealSamples.filter((sample) => sample.kind === "shallow").map((sample) => sample.ms);
  const deepReveal = revealSamples.filter((sample) => sample.kind === "deep").map((sample) => sample.ms);
  const cachedReveal = revealSamples.filter((sample) => sample.cached).map((sample) => sample.ms);

  await navigateAndProve(smokePage, shallowId, shallowMarker);
  const resizeNonce = crypto.randomUUID().replaceAll("-", "").slice(0, 12);
  const resize = await smokePage.evaluate(async ({ id, frame, marker }) => {
    const smoke = window.__smoke;
    const deck = document.querySelector('[data-testid="terminal-deck"]') as HTMLElement | null;
    if (!deck) throw new Error("terminal deck missing");
    const originalStyle = deck.getAttribute("style");
    const lastMark = smoke.phaseTimeline().marks.at(-1)?.index ?? -1;
    const timingId = await smoke.beginTerminalTiming("resize", id);
    const rect = deck.getBoundingClientRect();
    deck.style.width = `${Math.max(320, Math.round(rect.width - 160))}px`;
    deck.style.height = `${Math.max(220, Math.round(rect.height - 96))}px`;
    try {
      await new Promise<void>((resolve, reject) => {
        const deadline = performance.now() + 15_000;
        const check = () => {
          const accepted = smoke.phaseTimeline().marks.some((phase) =>
            phase.index > lastMark
            && phase.name === "viewport_accept"
            && phase.detail.sessionId === id);
          if (accepted) { resolve(); return; }
          if (performance.now() >= deadline) { reject(new Error("viewport acceptance timed out")); return; }
          requestAnimationFrame(check);
        };
        check();
      });
      await smoke.input(id, frame);
      const timing = await smoke.finishTerminalTiming(timingId, id, marker, 30_000);
      return { timing, dimensions: smoke.terminalDimensions(id) };
    } finally {
      if (originalStyle === null) deck.removeAttribute("style");
      else deck.setAttribute("style", originalStyle);
    }
  }, {
    id: shallowId,
    frame: encodePtyFixtureCommand({ op: "REPORT_SIZE", nonce: resizeNonce }),
    marker: `SIZE:${resizeNonce}:`,
  });
  const observedSize = new RegExp(`SIZE:${resizeNonce}:(\\d+)x(\\d+)`).exec(resize.timing.rowText);
  expect(observedSize).not.toBeNull();
  expect({ cols: Number(observedSize![1]), rows: Number(observedSize![2]) }).toEqual(resize.dimensions);

  const slot = smokePage.getByTestId(`terminal-slot-${shallowId}`);
  await slot.click();
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = window.__smoke;
    return smoke.paneFocused(id).focused;
  }, shallowId)).toBe(true);
  const keySamples: number[] = [];
  for (let sample = 0; sample < 45; sample++) {
    const nonce = `${sample}-${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}`;
    await smokePage.evaluate(async ({ id, frame, marker }) => {
      const smoke = window.__smoke;
      await smoke.input(id, frame);
      await smoke.waitForPaintedMarker(id, marker);
    }, {
      id: shallowId,
      frame: encodePtyFixtureCommand({ op: "ARM_KEY", nonce }),
      marker: `ARMED:${nonce}`,
    });
    const timingId = await smokePage.evaluate((id) => {
      const smoke = window.__smoke;
      return smoke.beginTerminalTiming("trusted_key", id);
    }, shallowId);
    await smokePage.keyboard.press("x");
    const result = await smokePage.evaluate(({ timingId, id, marker }) => {
      const smoke = window.__smoke;
      return smoke.finishTerminalTiming(timingId, id, marker);
    }, { timingId, id: shallowId, marker: `ACK:${nonce}` });
    expect(result.trustedKey).toBe(true);
    if (sample >= 5) keySamples.push(result.durationMs);
  }

  const interactionReport = {
    reveal_shallow_ms: shallowReveal,
    reveal_deep_ms: deepReveal,
    reveal_shallow_p95_ms: percentile(shallowReveal, 0.95),
    reveal_deep_p95_ms: percentile(deepReveal, 0.95),
    reveal_cached_css_only_ms: cachedReveal,
    resize_child_observed_ms: resize.timing.durationMs,
    trusted_key_ms: keySamples,
    trusted_key_p50_ms: percentile(keySamples, 0.5),
    trusted_key_p95_ms: percentile(keySamples, 0.95),
    trusted_key_max_ms: Math.max(...keySamples),
  };
  console.log(`[perf.interaction] ${JSON.stringify(interactionReport)}`);
  await testInfo.attach("perf-interaction.json", {
    body: JSON.stringify(interactionReport, null, 2),
    contentType: "application/json",
  });

  if (QUALIFY) {
    expect(interactionReport.reveal_shallow_p95_ms).toBeLessThanOrEqual(150);
    expect(Math.max(...shallowReveal)).toBeLessThanOrEqual(300);
    expect(interactionReport.reveal_deep_p95_ms).toBeLessThanOrEqual(150);
    expect(Math.max(...deepReveal)).toBeLessThanOrEqual(300);
    expect(interactionReport.reveal_deep_p95_ms - interactionReport.reveal_shallow_p95_ms).toBeLessThanOrEqual(50);
    expect(interactionReport.resize_child_observed_ms).toBeLessThanOrEqual(250);
    expect(interactionReport.trusted_key_p50_ms).toBeLessThanOrEqual(75);
    expect(interactionReport.trusted_key_p95_ms).toBeLessThanOrEqual(150);
    expect(interactionReport.trusted_key_max_ms).toBeLessThanOrEqual(300);
  }
}

export async function probeOptimisticPaint(
  { smokePage, stack }: { smokePage: Page; stack: TerminalTestStack },
  testInfo: TestInfo,
): Promise<void> {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop optimistic paint endpoint");
  test.setTimeout(120_000);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  const anchorId = await spawnFixtureSession(smokePage, fixtureWorker);
  await navigateAndProve(smokePage, anchorId, PTY_FIXTURE_READY);
  const oldIds = await smokePage.evaluate(() => Object.keys(window.__smoke.state().sessions));

  let releaseResponse!: () => void;
  let sawRequest!: () => void;
  let routeDone!: () => void;
  const release = new Promise<void>((resolve) => { releaseResponse = resolve; });
  const intercepted = new Promise<void>((resolve) => { sawRequest = resolve; });
  const finished = new Promise<void>((resolve) => { routeDone = resolve; });
  let responseReleased = false;
  let requestIntercepted = false;
  await smokePage.route("**/roost.v1.CoordinatorService/SessionsSpawn", async (route) => {
    requestIntercepted = true;
    sawRequest();
    try {
      const response = await route.fetch();
      await release;
      responseReleased = true;
      await route.fulfill({ response });
    } finally {
      routeDone();
    }
  });

  await installTerminalLoadingStageProbe(smokePage);

  const timingId = await smokePage.evaluate(() => window.__smoke.beginTerminalTiming("optimistic"));
  try {
    await smokePage.getByTestId("tab-new").first().click();
    await intercepted;
    await expect.poll(() => terminalLoadingStages(smokePage)).toContain("spawn");
    expect(responseReleased).toBe(false);
    await smokePage.waitForFunction((existing) => {
      const smoke = window.__smoke;
      return Object.keys(smoke.state().sessions).some((id) => !existing.includes(id));
    }, oldIds);
    const optimisticId = await smokePage.evaluate((existing) => {
      const smoke = window.__smoke;
      return Object.keys(smoke.state().sessions).find((id) => !existing.includes(id))!;
    }, oldIds);
    await smokePage.evaluate((id) => {
      const smoke = window.__smoke;
      smoke.trackCreatedSession(id);
    }, optimisticId);
    const timing = await smokePage.evaluate(({ timingId, id, marker }) => {
      const smoke = window.__smoke;
      return smoke.finishTerminalTiming(timingId, id, marker, 60_000);
    }, { timingId, id: optimisticId, marker: PTY_FIXTURE_READY });
    await expect(smokePage.getByTestId("terminal-loading-status")).toHaveCount(0);
    expect(responseReleased).toBe(false);
    expect(await smokePage.evaluate((id) => {
      const smoke = window.__smoke;
      return smoke.lastFullFrameSbRows(id);
    }, optimisticId)).toBe(0);
    await testInfo.attach("perf-optimistic.json", {
      body: JSON.stringify({ optimistic_marker_ms: timing.durationMs, response_released_at_paint: responseReleased }, null, 2),
      contentType: "application/json",
    });
  } finally {
    releaseResponse();
    if (requestIntercepted) await finished.catch(() => undefined);
    await smokePage.unroute("**/roost.v1.CoordinatorService/SessionsSpawn");
  }
}
