// Find in scrollback, end to end: real PTY history → worker grid search → coord
// RPC → highlights → the ONE new scrollTop writer.
//
// The jump is the risk. scrollToScrollbackRow is the only scrollTop writer
// besides _pinToBottom, and a bad jump is exactly the L11 scroll-lurch /
// history-corruption class — so this asserts the match row is really PAINTED
// (not blank spacer), the reader lands on it, and markerScan stays clean
// afterwards.

import { test, expect } from "./fixtures.ts";

type FindSmoke = {
  spawnShell(worker: string, folder: string): Promise<{ session_id: string }>;
  input(sessionId: string, text: string): Promise<void>;
  markerScan(sessionId: string, prefix: string): {
    max: number; duplicated: number[]; outOfOrder: number;
  };
  renderProbe(sessionId: string): { rowCount: number; atBottom: boolean };
};

const LINES = 3000;
// Deliberately below the held window (MAX_HELD_SCROLLBACK_ROWS = 2000 of a
// 3000-line history) so the jump MUST pull the row in via ensureRowPainted
// instead of landing on reserved-but-unpainted spacer. 400 is also unique as a
// substring: 4000 > LINES, so no longer marker contains "FINDLINE-400".
const TARGET = 400;

test("find in scrollback lands the reader on a painted match", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop scroll-geometry contract");
  test.setTimeout(240_000);

  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: FindSmoke }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();

  await smokePage.evaluate(async ({ id, lines }) => {
    const smoke = (window as unknown as Window & { __smoke: FindSmoke }).__smoke;
    await smoke.input(id, `seq -f 'FINDLINE-%g' 1 ${lines}\r`);
  }, { id: sessionId, lines: LINES });
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: FindSmoke }).__smoke;
    return smoke.markerScan(id, "FINDLINE-").max;
  }, sessionId), { timeout: 120_000 }).toBe(LINES);

  // FINDLINE-400 sits in the unpainted [0, sbBase) region, so this exercises the
  // ensureRowPainted pull before the jump.
  await smokePage.keyboard.press("Control+Shift+F");
  const input = smokePage.getByTestId("terminal-find-input");
  await expect(input).toBeVisible();
  await input.fill(`FINDLINE-${TARGET}`);

  // 1/1: the marker is unique in the history.
  await expect(smokePage.getByTestId("terminal-find-count")).toHaveText("1/1", { timeout: 30_000 });

  // The match row must be PAINTED and inside the viewport — not reserved spacer.
  // Polled because the jump first PULLS that row in (ensureRowPainted issues
  // scrollback RPCs and splices a slice per animation frame), so it lands a few
  // frames after the counter appears.
  const probeLanding = () => smokePage.evaluate(({ id, marker }) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const container = pane?.querySelector(".wterm") as HTMLElement | null;
    if (!container) return { found: false, visible: false, highlighted: false, computed: "" };
    const hit = container.querySelector(".cell-find-hit") as HTMLElement | null;
    if (!hit) return { found: false, visible: false, highlighted: false, computed: "" };
    const row = hit.closest(".cell-row") as HTMLElement | null;
    const box = row?.getBoundingClientRect();
    const view = container.getBoundingClientRect();
    return {
      found: (row?.textContent ?? "").includes(marker),
      visible: !!box && box.bottom > view.top && box.top < view.bottom,
      highlighted: hit.textContent === marker,
      computed: getComputedStyle(hit).backgroundColor,
    };
  }, { id: sessionId, marker: `FINDLINE-${TARGET}` });
  await expect.poll(async () => {
    const s = await probeLanding();
    return s.found && s.visible && s.highlighted;
  }, { timeout: 60_000, intervals: [250] }).toBe(true);
  // The highlight class must actually win over the run's inline style; a
  // transparent background means the match is painted but invisible.
  const landed = await probeLanding();
  expect(landed.computed).not.toBe("rgba(0, 0, 0, 0)");
  expect(landed.computed).not.toBe("transparent");

  // The jump must not corrupt history.
  const afterJump = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: FindSmoke }).__smoke;
    return smoke.markerScan(id, "FINDLINE-");
  }, sessionId);
  expect(afterJump).toMatchObject({ duplicated: [], outOfOrder: 0 });

  // Esc closes and hands the keyboard back to the PTY, and bottom-follow still
  // works: scrolling to the bottom resumes tracking new output.
  await smokePage.keyboard.press("Escape");
  await expect(input).toHaveCount(0);
  await smokePage.evaluate((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const container = pane?.querySelector(".wterm") as HTMLElement | null;
    if (container) container.scrollTop = container.scrollHeight;
  }, sessionId);
  const tail = `FINDTAIL-${Date.now() % 100000}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${tail}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 30_000 }).toContain(tail);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: FindSmoke }).__smoke;
    return smoke.renderProbe(id).atBottom;
  }, sessionId)).toBe(true);

  const finalScan = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: FindSmoke }).__smoke;
    return smoke.markerScan(id, "FINDLINE-");
  }, sessionId);
  expect(finalScan).toMatchObject({ duplicated: [], outOfOrder: 0 });
});
