import { fileURLToPath } from "node:url";
import { test, expect } from "./fixtures.ts";
import { dirname, join } from "node:path";

const fixturePath = join(dirname(fileURLToPath(import.meta.url)), "resize-tui.ts");
test("browser smoke flow creates and cleans its resources", async ({ smokePage }) => {
  const result = await smokePage.evaluate(async () => {
    const smoke = (window as unknown as Window & { __smoke: { runFlow(): Promise<{ steps: Array<{ pass: boolean }>; summary: string }> } }).__smoke;
    return smoke.runFlow();
  });
  expect(result.steps.filter((step) => !step.pass)).toEqual([]);
});

test("terminal replay and Ctrl keys stay owned by the PTY", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop keyboard contract");
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();

  await smokePage.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });
  const marker = `NATIVE_FOCUS_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${marker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain(marker);

  const sidebar = smokePage.getByTestId("sidebar-desktop");
  const collapsed = await sidebar.getAttribute("data-collapsed");
  await smokePage.keyboard.type("cat -v");
  await smokePage.keyboard.press("Enter");
  await smokePage.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });

  await smokePage.keyboard.down("Control");
  await smokePage.keyboard.press("b");
  await smokePage.keyboard.press("f");
  await smokePage.keyboard.up("Control");

  await smokePage.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });

  await smokePage.keyboard.down("Control");
  await smokePage.keyboard.press("k");
  await smokePage.keyboard.up("Control");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain("^B^F^K");
  await expect(sidebar).toHaveAttribute("data-collapsed", collapsed ?? "false");
  await expect(smokePage.getByTestId("sidebar-search")).toHaveCount(0);
  await expect(smokePage.getByTestId("command-palette")).toHaveCount(0);
  await smokePage.keyboard.press("Control+C");
});

test("nav pad taps reach the PTY without focusing the terminal textarea", async ({ mobileSmokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "webkit-iphone", "iPhone terminal input contract");
  const sessionId = await mobileSmokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await mobileSmokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();

  await mobileSmokePage.keyboard.type("cat -vet");
  await mobileSmokePage.keyboard.press("Enter");
  await mobileSmokePage.getByTestId("terminal-nav-toggle").click();

  const paneFocused = () => mobileSmokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        paneFocused(sessionId: string): {
          hasSlot: boolean;
          hasTextarea: boolean;
          focused: boolean;
        };
      };
    }).__smoke;
    return smoke.paneFocused(id);
  }, sessionId);
  await mobileSmokePage.evaluate(() => {
    (document.activeElement as HTMLElement | null)?.blur();
    document.body.tabIndex = -1;
    document.body.focus();
  });
  await expect.poll(paneFocused).toMatchObject({ hasTextarea: true, focused: false });

  const clickWithoutFocus = async (testId: string) => {
    await mobileSmokePage.getByTestId(testId).click();
    await expect.poll(async () => (await paneFocused()).focused).toBe(false);
  };
  for (const testId of [
    "nav-up",
    "nav-down",
    "nav-left",
    "nav-right",
    "nav-home",
    "nav-end",
    "nav-pgup",
    "nav-pgdn",
    "nav-esc",
    "nav-tab",
    "nav-mouse",
    "nav-mouse",
    "nav-ctrl",
    "nav-ctrl",
    "terminal-nav-toggle",
    "terminal-nav-toggle",
  ]) {
    await clickWithoutFocus(testId);
  }
  await expect(mobileSmokePage.getByTestId("nav-mouse")).toHaveAttribute("aria-pressed", "false");
  await expect(mobileSmokePage.getByTestId("nav-ctrl")).toHaveAttribute("aria-pressed", "false");
  await expect(mobileSmokePage.getByTestId("terminal-nav-toggle")).toHaveAttribute("data-open", "true");

  await clickWithoutFocus("nav-enter");
  await expect.poll(() => slot.textContent()).toContain("^I$");
});

test("mobile composer preserves input and the Ctrl pad interrupts", async ({ mobileSmokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "webkit-iphone", "iPhone terminal input contract");
  const sessionId = await mobileSmokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await mobileSmokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();

  await slot.click();
  await mobileSmokePage.keyboard.type(`IFS= read -r line; printf '<%s>\\n' "$line"`);
  await mobileSmokePage.keyboard.press("Enter");
  await mobileSmokePage.getByTestId("terminal-chat-toggle").click();
  await mobileSmokePage.getByTestId("chat-input").fill("  x  ");
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect.poll(() => slot.textContent()).toContain("<  x  >");

  await slot.click();
  await mobileSmokePage.keyboard.type(`IFS= read -r line; printf '<%s>\\n' "$line"`);
  await mobileSmokePage.keyboard.press("Enter");
  await mobileSmokePage.getByTestId("terminal-chat-toggle").click();
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect.poll(() => slot.textContent()).toContain("<>");

  await slot.click();
  await mobileSmokePage.keyboard.type("sleep 30");
  await mobileSmokePage.keyboard.press("Enter");
  await mobileSmokePage.getByTestId("terminal-nav-toggle").click();
  await expect(mobileSmokePage.getByTestId("nav-tab")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("nav-home")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("nav-end")).toBeVisible();
  const ctrl = mobileSmokePage.getByTestId("nav-ctrl");
  await expect(ctrl).toHaveAttribute("aria-pressed", "false");
  await ctrl.click();
  await expect(ctrl).toHaveAttribute("aria-pressed", "true");
  await mobileSmokePage.getByTestId("terminal-nav-toggle").click();
  await mobileSmokePage.getByTestId("terminal-nav-toggle").click();
  await expect(mobileSmokePage.getByTestId("nav-ctrl")).toHaveAttribute("aria-pressed", "false");
  await mobileSmokePage.getByTestId("nav-ctrl").click();
  await expect.poll(() => mobileSmokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { paneFocused(sessionId: string): { focused: boolean } };
    }).__smoke;
    return smoke.paneFocused(id).focused;
  }, sessionId)).toBe(false);
  await slot.click({ position: { x: 8, y: 8 } });
  await expect.poll(() => mobileSmokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { paneFocused(sessionId: string): { focused: boolean } };
    }).__smoke;
    return smoke.paneFocused(id).focused;
  }, sessionId)).toBe(true);
  await mobileSmokePage.keyboard.type("c");
  const marker = `TOUCH_CTRL_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await mobileSmokePage.keyboard.type(`printf '%s\\n' ${marker}`);
  await mobileSmokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain(marker);
});

test("trusted keyboard input and bottom-follow behavior", async ({ smokePage, stack }) => {
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  const marker = `PW_INPUT_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${marker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain(marker);

  await smokePage.keyboard.type("for i in $(seq 1 500); do echo BOTTOMLINE-$i; done");
  await smokePage.keyboard.press("Enter");
  await expect.poll(async () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { renderProbe(sessionId: string): { rowCount: number; atBottom: boolean } } }).__smoke;
    return smoke.renderProbe(id);
  }, sessionId)).toMatchObject({ atBottom: true });
});

test("alternate screen survives width and height perturbations", async ({ smokePage, stack }) => {
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
  await smokePage.keyboard.type(`'${fixturePath}'`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => smokePage.getByTestId(`terminal-slot-${sessionId}`).textContent()).toContain("CELLLINE-60");
  const result = await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        runRenderStress(options: {
          sessionId: string; prefix: string; screen: "main" | "alt"; iterations: number;
        }): Promise<{ verdict: string; fails: unknown[] }>;
      };
    }).__smoke;
    return smoke.runRenderStress({ sessionId: id, prefix: "CELLLINE-", screen: "alt", iterations: 80 });
  }, sessionId);
  expect(result).toMatchObject({ verdict: "PASS", fails: [] });
  await smokePage.keyboard.press("q");
});

test("two viewers preserve ordered terminal markers", async ({ smokePage, browser, stack }) => {
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  await smokePage.keyboard.type("for i in $(seq 1 120); do echo MULTIVIEW-$i; done");
  await smokePage.keyboard.press("Enter");
  const passiveContext = await browser.newContext({ viewport: { width: 1400, height: 900 } });
  await passiveContext.addInitScript(() => localStorage.setItem("roostSmoke", "1"));
  const passive = await passiveContext.newPage();
  try {
    await passive.goto(`${stack.baseUrl}/s/${sessionId}`);
    await passive.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
    await passive.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(true));
    await smokePage.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(true));
    await smokePage.bringToFront();
    for (let iteration = 0; iteration < 24; iteration++) {
      await smokePage.setViewportSize({ width: 700 + (iteration % 2) * 50, height: 500 + (iteration % 3) * 40 });
    }
    const scan = await passive.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: { markerScan(sessionId: string, prefix: string): unknown } }).__smoke;
      return smoke.markerScan(id, "MULTIVIEW-");
    }, sessionId);
    expect(scan).toMatchObject({ duplicated: [], outOfOrder: 0 });
    await passive.bringToFront();
    for (let iteration = 0; iteration < 24; iteration++) {
      await passive.setViewportSize({ width: 1350 + (iteration % 2) * 50, height: 820 + (iteration % 3) * 40 });
    }
    await smokePage.bringToFront();
    const primaryScan = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: { markerScan(sessionId: string, prefix: string): unknown } }).__smoke;
      return smoke.markerScan(id, "MULTIVIEW-");
    }, sessionId);
    expect(primaryScan).toMatchObject({ duplicated: [], outOfOrder: 0 });
  } finally {
    await smokePage.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(false)).catch(() => undefined);
    await passive.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(false)).catch(() => undefined);
    await passiveContext.close();
  }
});

// The scroll SPACE, in a real browser. A fresh attach to a deep session gets a
// scrollback TAIL (SB_SNAPSHOT_TAIL_ROWS) plus sbBase; before
// CellGridRenderer._syncSpacer nothing stood in for [0, sbBase), so
// scrollHeight described ~250 rows of an 8000-row session and every backfill
// prepend grew it — the thumb shrank and jumped with no user action, and a
// reader parked in history drifted onto other rows. This asserts the two
// properties the spacer buys: the scroll space is truthful on attach, and it
// does not move while the drain paints history in under the reader.
test("a deep-history attach reserves the whole scroll space and holds it through the backfill drain", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop scroll-geometry contract");
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
  await smokePage.keyboard.type("seq -f 'CELLLINE-%g' 1 8000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => smokePage.getByTestId(`terminal-slot-${sessionId}`).textContent(), { timeout: 60_000 })
    .toContain("CELLLINE-8000");

  // Re-attach: the worker now answers with a tail + a deep sbBase, which is the
  // exact state the old renderer misrepresented.
  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await smokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  await expect(smokePage.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
  await smokePage.waitForFunction((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    return !!slot?.querySelector(".cell-sb-spacer") && !!slot.querySelector(".cell-row");
  }, sessionId);
  const geometry = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { renderProbe(sessionId: string): { rowCount: number; scrollHeight: number; clientHeight: number } } }).__smoke;
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slot?.querySelector(".wterm") as HTMLElement | null;
    const rowH = c?.querySelector(".cell-row")?.getBoundingClientRect().height ?? 0;
    const spacer = c?.querySelector(".cell-sb-spacer") as HTMLElement | null;
    const probe = smoke.renderProbe(id);
    // The marker on the topmost row whose box still intersects the viewport top
    // — textContent can't answer this (content-visibility-skipped blocks are in
    // it), and it is precisely "what row is the reader looking at".
    const top = c?.getBoundingClientRect().top ?? 0;
    const reader = Array.from(c?.querySelectorAll(".cell-row") ?? [])
      .map((r) => [r.getBoundingClientRect(), r.textContent ?? ""] as const)
      .filter(([b]) => b.bottom > top + 1)
      .map(([, t]) => (t.match(/CELLLINE-\d+/) ?? [null])[0])[0] ?? null;
    return { rowH, spacerPx: spacer ? parseFloat(spacer.style.height || "0") : -1, reader, ...probe };
  }, sessionId);

  const attach = await geometry();
  expect(attach.rowH).toBeGreaterThan(0);
  // Truthful depth: the space covers ~8000 rows even though only a tail is painted.
  expect(attach.spacerPx / attach.rowH).toBeGreaterThan(5000);
  expect(attach.scrollHeight / attach.rowH).toBeGreaterThan(7000);
  expect(attach.rowCount).toBeLessThan(3000); // still just a tail in the DOM

  // Park the reader ten rows into the PAINTED tail: close enough that
  // nearHistoryTop() stays true (so the drain keeps prepending chunks ABOVE the
  // reader, which is the mutation under test), and still a real painted row at
  // the viewport top so "the reader's row" is well-defined for the whole drain.
  // Every prepended chunk must shrink the spacer by exactly its own height, or
  // this row moves.
  await smokePage.evaluate(({ id, rowH }) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slot?.querySelector(".wterm") as HTMLElement | null;
    const sb = c?.querySelector(".cell-scrollback") as HTMLElement | null;
    if (c && sb) c.scrollTop = sb.offsetTop + Math.round(10 * rowH);
  }, { id: sessionId, rowH: attach.rowH });
  const parked = await geometry();
  expect(parked.reader).toMatch(/^CELLLINE-\d+$/);

  // Drain to quiescence: rowCount stops growing once the painted head passes
  // the reader. Neither the scroll space nor the reader's row may move.
  let last = -1, settled = 0;
  await expect.poll(async () => {
    const g = await geometry();
    settled = g.rowCount === last ? settled + 1 : 0;
    last = g.rowCount;
    return settled;
  }, { timeout: 60_000, intervals: [250] }).toBeGreaterThanOrEqual(4);

  const drained = await geometry();
  expect(drained.rowCount).toBeGreaterThan(attach.rowCount); // the drain really ran
  expect(Math.abs(drained.scrollHeight - parked.scrollHeight)).toBeLessThanOrEqual(Math.ceil(drained.rowH));
  expect(drained.reader).toBe(parked.reader);
  expect(drained.scrollTop).toBe(parked.scrollTop); // zero application scroll writes
});
