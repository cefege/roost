import { expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";
import type { TerminalStreamProbe } from "../../apps/web/src/lib/smoke.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import type { PaintedScrollbackProbe, RecoverySmokeApi } from "./terminal-smoke-api.ts";
import { inputSmokeTerminal } from "./terminal-helpers.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";
const WIDE_SHORT_VIEWPORT = { width: 1_440, height: 640 };
const NARROW_TALL_VIEWPORT = { width: 900, height: 960 };
const RESIZED_WIDE_VIEWPORT = { width: 1_300, height: 780 };
const RESIZED_NARROW_VIEWPORT = { width: 1_060, height: 880 };

const PERCENT_ROW = 3;
const PERCENT_COL = 19;
const VALUE_ROW = 4;
const VALUE_COL = 19;
const ACTIVITY_ROW = 6;
const ACTIVITY_COL = 40;
const FOOTER_ROW = 12;
const FIRST_INPUT_ROW = 14;
const SECOND_INPUT_ROW = 15;

type EffectiveGeometry = { cols: number; rows: number };
type TerminalControl = EffectiveGeometry & { activeViewCount: number; streamId: string };

type TransitionOptions = {
  activeIndices: readonly number[];
  /** Browser intents that remain active even if their Sync transport is absent. */
  desiredActiveIndices?: readonly number[];
  activeViewCount: number;
  streamId?: string;
  previousStreamId?: string;
  /** Per-browser retained stream IDs for replicas intentionally offline. */
  replicaStreamIds?: readonly (string | undefined)[];
  geometry?: EffectiveGeometry;
  geometryPredicate?: (geometry: EffectiveGeometry) => boolean;
  domMayLag?: readonly number[];
};

type Transition = {
  probes: TerminalStreamProbe[];
  control: TerminalControl;
};

function record(value: unknown, label: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} was not an object`);
  }
  return value as Record<string, unknown>;
}

function integer(value: unknown, label: string, minimum = 0): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < minimum) {
    throw new Error(`${label} was not an integer >= ${minimum}`);
  }
  return value;
}

function finiteNumber(value: unknown, label: string, minimum = 0): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < minimum) {
    throw new Error(`${label} was not a finite number >= ${minimum}`);
  }
  return value;
}

function nonEmptyString(value: unknown, label: string): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`${label} was not a non-empty string`);
  }
  return value;
}

function terminalControl(probe: TerminalStreamProbe): TerminalControl {
  const control = record(probe.coord?.terminal_control, "coordinator terminal control");
  return {
    activeViewCount: integer(control.active_view_count, "active view count"),
    streamId: nonEmptyString(control.stream_id, "coordinator stream ID"),
    cols: integer(control.effective_cols, "coordinator effective columns", 1),
    rows: integer(control.effective_rows, "coordinator effective rows", 1),
  };
}

function assertTransition(probes: TerminalStreamProbe[], options: TransitionOptions): TerminalControl {
  expect(probes.length).toBeGreaterThan(0);
  const controls = probes.map(terminalControl);
  const control = controls[0]!;
  for (const candidate of controls.slice(1)) expect(candidate).toEqual(control);

  expect(control.activeViewCount).toBe(options.activeViewCount);
  if (options.streamId !== undefined) expect(control.streamId).toBe(options.streamId);
  if (options.previousStreamId !== undefined) {
    expect(control.streamId).not.toBe(options.previousStreamId);
  }
  if (options.geometry !== undefined) {
    expect({ cols: control.cols, rows: control.rows }).toEqual(options.geometry);
  }
  if (options.geometryPredicate !== undefined) {
    expect(options.geometryPredicate({ cols: control.cols, rows: control.rows })).toBe(true);
  }

  const coordinatorActive = new Set(options.activeIndices);
  const desiredActive = new Set(options.desiredActiveIndices ?? options.activeIndices);
  const domMayLag = new Set(options.domMayLag ?? []);
  for (const [index, probe] of probes.entries()) {
    const { view, replica, dom_reconciled: dom } = probe.browser;
    expect(replica.expected_stream_id).toBe(options.replicaStreamIds?.[index] ?? control.streamId);
    expect(replica.baseline_ready).toBe(true);
    expect(replica.resync_latched).toBe(false);
    const replicaEpoch = nonEmptyString(replica.grid_epoch, `browser ${index} replica epoch`);
    const replicaSeq = integer(replica.seq, `browser ${index} replica sequence`);
    const domEpoch = nonEmptyString(dom.grid_epoch, `browser ${index} DOM epoch`);
    const domSeq = integer(dom.seq, `browser ${index} DOM sequence`);

    if (domMayLag.has(index)) {
      if (domEpoch === replicaEpoch) expect(domSeq).toBeLessThanOrEqual(replicaSeq);
    } else {
      expect({ grid_epoch: domEpoch, seq: domSeq }).toEqual({
        grid_epoch: replicaEpoch,
        seq: replicaSeq,
      });
    }

    if (desiredActive.has(index)) {
      expect(view.active).toBe(true);
      nonEmptyString(view.view_id, `browser ${index} view ID`);
      nonEmptyString(view.revision, `browser ${index} view revision`);
      nonEmptyString(view.status, `browser ${index} view status`);
      finiteNumber(view.lease_deadline_ms, `browser ${index} view lease deadline`, 1);
      if (coordinatorActive.has(index)) {
        expect(view.stream_id).toBe(control.streamId);
        expect({ cols: view.effective_cols, rows: view.effective_rows }).toEqual({
          cols: control.cols,
          rows: control.rows,
        });
      }
    } else {
      expect(view.active).toBe(false);
    }
  }
  const activeWatermarks = [...coordinatorActive].map((index) => {
    const replica = probes[index]!.browser.replica;
    return {
      grid_epoch: nonEmptyString(replica.grid_epoch, `active browser ${index} replica epoch`),
      seq: integer(replica.seq, `active browser ${index} replica sequence`),
    };
  });
  for (const watermark of activeWatermarks.slice(1)) expect(watermark).toEqual(activeWatermarks[0]);
  expect(coordinatorActive.size).toBe(options.activeViewCount);
  return control;
}

async function waitForTransition(
  pages: readonly Page[],
  sessionId: string,
  options: TransitionOptions,
): Promise<Transition> {
  let transition: Transition | null = null;
  await expect.poll(async () => {
    const probes = await Promise.all(pages.map((page) => readTerminalStreamProbe(page, sessionId)));
    try {
      const control = assertTransition(probes, options);
      transition = { probes, control };
      return "ready";
    } catch (error) {
      return `${error instanceof Error ? error.message : String(error)}; probes=${JSON.stringify(probes)}`;
    }
  }, { timeout: 45_000, intervals: [50, 100, 250] }).toBe("ready");
  if (transition === null) throw new Error("terminal transition completed without a snapshot");
  return transition;
}

function expectedMinimum(...geometries: EffectiveGeometry[]): EffectiveGeometry {
  return {
    cols: Math.min(...geometries.map((geometry) => geometry.cols)),
    rows: Math.min(...geometries.map((geometry) => geometry.rows)),
  };
}

function waitForPainted(page: Page, sessionId: string, marker: string) {
  return page.evaluate(({ id, text }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, text, 30_000);
  }, { id: sessionId, text: marker });
}

function readPaintedScrollback(page: Page, sessionId: string): Promise<PaintedScrollbackProbe> {
  return page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.paintedScrollback(id);
  }, sessionId);
}
async function readBackfillRequestCount(page: Page, sessionId: string): Promise<number> {
  const count = await page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.scrollbackBackfillRequestCount(id);
  }, sessionId);
  return integer(count, "scrollback backfill request count");
}

function assertPaintedScrollback(probe: PaintedScrollbackProbe): void {
  finiteNumber(probe.headSpacerPx, "painted scrollback head spacer");
  finiteNumber(probe.tailGapPx, "painted scrollback tail gap");
  const indices = probe.rows.map((row) => integer(row.index, "painted global row index"));
  expect(new Set(indices).size).toBe(indices.length);
  expect(indices).toEqual([...indices].sort((left, right) => left - right));
  for (const row of probe.rows) expect(typeof row.text).toBe("string");
  if (probe.readerAnchor !== null) {
    integer(probe.readerAnchor.row, "reader anchor row");
    finiteNumber(probe.readerAnchor.offsetPx, "reader anchor offset");
  }
}

async function waitForHistoryAnchor(
  page: Page,
  sessionId: string,
  historyPrefix: string,
): Promise<{ row: number; text: string; offsetPx: number }> {
  await expect.poll(async () => {
    const [stream, painted] = await Promise.all([
      readTerminalStreamProbe(page, sessionId),
      readPaintedScrollback(page, sessionId),
    ]);
    const anchor = painted.readerAnchor;
    const row = anchor === null ? undefined : painted.rows.find((candidate) => candidate.index === anchor.row);
    return {
      intent: stream.browser.presentation?.reader_intent ?? null,
      anchoredHistory: row?.text.includes(historyPrefix) ?? false,
    };
  }, { timeout: 30_000, intervals: [50, 100, 250] }).toEqual({
    intent: "reading",
    anchoredHistory: true,
  });

  const painted = await readPaintedScrollback(page, sessionId);
  assertPaintedScrollback(painted);
  expect(painted.headSpacerPx).toBeGreaterThan(0);
  if (painted.readerAnchor === null) throw new Error("reader omitted its global anchor");
  const row = painted.rows.find((candidate) => candidate.index === painted.readerAnchor!.row);
  if (!row) throw new Error("reader anchor was outside the painted scrollback rows");
  return { row: row.index, text: row.text, offsetPx: painted.readerAnchor.offsetPx };
}

async function expectHistoryAnchorPreserved(
  page: Page,
  sessionId: string,
  expectedAnchor: { row: number; text: string; offsetPx: number },
): Promise<void> {
  await expect.poll(async () => {
    const painted = await readPaintedScrollback(page, sessionId);
    const row = painted.rows.find((candidate) => candidate.index === expectedAnchor.row);
    return {
      anchorRow: painted.readerAnchor?.row ?? null,
      anchorOffsetPreserved: painted.readerAnchor !== null
        && Math.abs(painted.readerAnchor.offsetPx - expectedAnchor.offsetPx) <= 1,
      anchorText: row?.text ?? null,
    };
  }, { timeout: 30_000, intervals: [50, 100, 250] }).toEqual({
    anchorRow: expectedAnchor.row,
    anchorOffsetPreserved: true,
    anchorText: expectedAnchor.text,
  });
  const painted = await readPaintedScrollback(page, sessionId);
  assertPaintedScrollback(painted);
  expect(painted.headSpacerPx).toBeGreaterThan(0);
}

async function forceVisible(page: Page, visible: boolean): Promise<void> {
  await page.evaluate((on) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.forceVisible(on);
  }, visible);
}

async function forceHidden(page: Page, hidden: boolean): Promise<void> {
  await page.evaluate((on) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.forceHidden(on);
  }, hidden);
}

async function viewportText(page: Page, sessionId: string): Promise<string> {
  return page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.viewportText(id);
  }, sessionId);
}

async function expectMarkersOnce(page: Page, sessionId: string, markers: readonly string[]): Promise<void> {
  const text = await viewportText(page, sessionId);
  for (const marker of markers) expect(text.split(marker).length - 1).toBe(1);
  expect(text).not.toContain("FIXTURE_ERROR:");
}

function htopFrame(suffix: string): { payload: string; markers: string[] } {
  const markers = [
    `CPU-${suffix}`,
    `MEM-${suffix}`,
    `PROCS-${suffix}`,
    `+--BORDER-${suffix}--+`,
    `PID-${suffix}-USER-CPU%-MEM%-COMMAND`,
    `F1HELP-${suffix}`,
  ];
  const paints = [
    `\x1b[2J\x1b[${PERCENT_ROW};1H${markers[0]}\x1b[${PERCENT_ROW};18H[00.0]`,
    `\x1b[${VALUE_ROW};1H${markers[1]}\x1b[${VALUE_ROW};18H[0000M]`,
    `\x1b[${ACTIVITY_ROW};1H${markers[2]}\x1b[${ACTIVITY_ROW};${ACTIVITY_COL}H0`,
    `\x1b[7;1H${markers[3]}`,
    `\x1b[8;1H${markers[4]}`,
    `\x1b[${FOOTER_ROW};1H\x1b[7m ${markers[5]}  F2SETUP  F3SEARCH  F10QUIT \x1b[27m`,
  ];
  return {
    markers,
    payload: paints
      .map((text) => encodePtyFixtureCommand({ op: "EMIT", text, newline: false }))
      .join(""),
  };
}

function numericUpdate(value: string, percent: string, activity: string): string {
  if (!/^\d{4}$/.test(value) || !/^\d{2}\.\d$/.test(percent) || !/^\d$/.test(activity)) {
    throw new Error("alternate-screen updates must contain numeric tokens only");
  }
  return encodePtyFixtureCommand({
    op: "EMIT",
    newline: false,
    text: `\x1b[${VALUE_ROW};${VALUE_COL}H${value}`
      + `\x1b[${PERCENT_ROW};${PERCENT_COL}H${percent}`
      + `\x1b[${ACTIVITY_ROW};${ACTIVITY_COL}H${activity}`,
  });
}

async function applyNumericUpdate(
  sender: Page,
  viewers: readonly Page[],
  sessionId: string,
  htopMarkers: readonly string[],
  value: string,
  percent: string,
  activity: string,
): Promise<void> {
  await inputSmokeTerminal(sender, sessionId, numericUpdate(value, percent, activity));
  await Promise.all(viewers.map((page) => waitForPainted(page, sessionId, value)));
  await Promise.all(viewers.map((page) => expectMarkersOnce(page, sessionId, htopMarkers)));
}

export {
  ACTIVITY_COL, FIRST_INPUT_ROW, NARROW_TALL_VIEWPORT, RESIZED_NARROW_VIEWPORT,
  RESIZED_WIDE_VIEWPORT, SECOND_INPUT_ROW, WIDE_SHORT_VIEWPORT, applyNumericUpdate,
  expectHistoryAnchorPreserved, expectMarkersOnce, expectedMinimum, forceHidden,
  forceVisible, htopFrame, integer, nonEmptyString, numericUpdate,
  readBackfillRequestCount, viewportText, waitForHistoryAnchor, waitForPainted,
  waitForTransition,
};
