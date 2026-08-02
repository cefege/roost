// Terminal perf baseline. Measures the two numbers a user feels — how long a
// cold load takes to paint a first row, and how much main-thread jank a large
// output flood costs — and asserts only the two STRUCTURAL ceilings that a
// regression breaches but jitter cannot. Every other figure is printed so a
// before/after run can be compared in a PR body.
//
// Convention (same as terminal.spec.ts's reveal tests): assert the ABSENCE of
// work, never a wall-clock budget. A CI runner's wall clock is noise; a full
// frame per flood or half the flood spent in long tasks is a real defect.

import { test, expect } from "./fixtures.ts";

type PerfProbe = {
  longTaskCount: number; longTaskMs: number;
  cellFrames: number; cellFullFrames: number;
  domNodes: number; cellRows: number; heldSbRows: number; heapMb: number;
  inputRttP50: number; inputRttP95: number;
};

type PerfSmoke = {
  spawnShell(worker: string, folder: string): Promise<{ session_id: string }>;
  renderProbe(sessionId: string): { nonEmptyRows: number };
  markerScan(sessionId: string, prefix: string): { max: number; duplicated: number[]; outOfOrder: number };
  input(sessionId: string, text: string): Promise<void>;
  perfProbe(sessionId: string): PerfProbe;
  resetPerfCounters(): void;
};

const FLOOD_LINES = 20_000;

test("terminal perf baseline: cold first row and a 20k-line flood", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop perf budget");
  test.setTimeout(300_000);

  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: PerfSmoke }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);

  // Cold first row: a fresh document load straight onto the pane URL, timed
  // from navigation commit to the first non-empty painted row. This is the
  // whole bootstrap waterfall — identity, lists, Sync dial, claim, first frame.
  const coldStart = Date.now();
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`, { waitUntil: "commit" });
  await smokePage.waitForFunction((id) => {
    const smoke = (window as unknown as Window & { __smoke?: PerfSmoke }).__smoke;
    return !!smoke && smoke.renderProbe(id).nonEmptyRows > 0;
  }, sessionId, { timeout: 60_000 });
  const coldFirstRowMs = Date.now() - coldStart;

  const before = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: PerfSmoke }).__smoke;
    smoke.resetPerfCounters();
    return smoke.perfProbe(id);
  }, sessionId);

  const floodStart = Date.now();
  await smokePage.evaluate(async ({ id, lines }) => {
    const smoke = (window as unknown as Window & { __smoke: PerfSmoke }).__smoke;
    await smoke.input(id, `seq -f 'PERFLINE-%g' 1 ${lines}\r`);
  }, { id: sessionId, lines: FLOOD_LINES });
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: PerfSmoke }).__smoke;
    return smoke.markerScan(id, "PERFLINE-").max;
  }, sessionId), { timeout: 180_000, intervals: [250] }).toBe(FLOOD_LINES);
  const floodWallMs = Date.now() - floodStart;

  const after = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: PerfSmoke }).__smoke;
    return smoke.perfProbe(id);
  }, sessionId);
  const scan = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: PerfSmoke }).__smoke;
    return smoke.markerScan(id, "PERFLINE-");
  }, sessionId);

  const report = {
    cold_first_row_ms: coldFirstRowMs,
    flood_wall_ms: floodWallMs,
    flood_long_task_count: after.longTaskCount,
    flood_long_task_ms: after.longTaskMs,
    flood_cell_frames: after.cellFrames - before.cellFrames,
    flood_cell_full_frames: after.cellFullFrames - before.cellFullFrames,
    dom_nodes: after.domNodes,
    cell_rows: after.cellRows,
    held_sb_rows: after.heldSbRows,
    heap_mb: after.heapMb,
    input_rtt_p50: after.inputRttP50,
    input_rtt_p95: after.inputRttP95,
  };
  console.log(`[perf] ${JSON.stringify(report)}`);
  await testInfo.attach("perf.json", { body: JSON.stringify(report, null, 2), contentType: "application/json" });

  // Ceiling 1: a flood must not spend most of its wall clock in main-thread
  // long tasks. Breaching this means per-chunk work grew super-linearly, which
  // is exactly the class the paint/worker phases delete.
  expect(after.longTaskMs).toBeLessThan(0.5 * floodWallMs);
  // Ceiling 2: streaming output is deltas. A full frame during a flood means a
  // reframe loop (dims churn, scrollback-shrink misdetect) — the corruption
  // class, not a slow frame. Three allows for the shell's own settling.
  expect(report.flood_cell_full_frames).toBeLessThanOrEqual(3);
  // The flood is also a correctness sample: measuring must never come at the
  // cost of the history invariants the whole model exists to protect.
  expect(scan).toMatchObject({ duplicated: [], outOfOrder: 0 });
});
