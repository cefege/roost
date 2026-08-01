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
  test.setTimeout(240_000);
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

// A fresh or bottom-following reattach must paint only the live tail while the
// spacer reserves the complete history depth. Older rows materialize only when
// the reader approaches the painted boundary, without moving that reader.
test("deep-history attach/reveal paints the live tail until history is requested", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop scroll-geometry contract");
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'CELLLINE-%g' 1 8000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 60_000 }).toContain("CELLLINE-8000");

  // Re-attach to force the worker's standard tail snapshot over deep history.
  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await smokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  await expect(slot).toBeVisible();
  await smokePage.waitForFunction((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    return !!pane?.querySelector(".cell-sb-spacer") && !!pane.querySelector(".cell-row");
  }, sessionId);

  const geometry = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): {
          rowCount: number;
          scrollTop: number;
          scrollHeight: number;
          clientHeight: number;
          atBottom: boolean;
        };
        markerScan(sessionId: string, prefix: string): {
          max: number;
          duplicated: number[];
          outOfOrder: number;
        };
      };
    }).__smoke;
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const container = pane?.querySelector(".wterm") as HTMLElement | null;
    const rowH = container?.querySelector(".cell-row")?.getBoundingClientRect().height ?? 0;
    const spacer = container?.querySelector(".cell-sb-spacer") as HTMLElement | null;
    const probe = smoke.renderProbe(id);
    // The topmost row whose box intersects the viewport top is the row the
    // reader is actually inspecting; textContent includes skipped rows.
    const top = container?.getBoundingClientRect().top ?? 0;
    const reader = Array.from(container?.querySelectorAll(".cell-row") ?? [])
      .map((row) => [row.getBoundingClientRect(), row.textContent ?? ""] as const)
      .filter(([box]) => box.bottom > top + 1)
      .map(([, text]) => (text.match(/CELLLINE-\d+/) ?? [null])[0])[0] ?? null;
    return {
      rowH,
      spacerPx: spacer ? parseFloat(spacer.style.height || "0") : -1,
      reader,
      markerMax: smoke.markerScan(id, "CELLLINE-").max,
      ...probe,
    };
  }, sessionId);

  const attach = await geometry();
  expect(attach.markerMax).toBe(8000);
  expect(attach.atBottom).toBe(true);
  expect(attach.rowH).toBeGreaterThan(0);
  expect(attach.spacerPx / attach.rowH).toBeGreaterThan(5000);
  expect(attach.scrollHeight / attach.rowH).toBeGreaterThan(7000);
  expect(attach.rowCount).toBeLessThan(500);

  // Five samples span five times the deleted 300 ms eager-backfill delay.
  const attachRowSamples: number[] = [];
  for (let i = 0; i < 5; i++) {
    await smokePage.waitForTimeout(300);
    attachRowSamples.push((await geometry()).rowCount);
  }
  expect(attachRowSamples).toEqual(Array(5).fill(attach.rowCount));

  // The freshly attached tail is immediately focused and usable through the
  // real textarea/keyboard path.
  const ready = `READY-${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${ready}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain(ready);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { paneFocused(sessionId: string): { focused: boolean } };
    }).__smoke;
    return smoke.paneFocused(id).focused;
  }, sessionId)).toBe(true);
  await expect.poll(async () => (await geometry()).atBottom).toBe(true);

  // Select five same-folder siblings in sequence. A becomes fifth in parked
  // recency, outside the four-pane background stream LRU.
  const siblings: string[] = [];
  for (let i = 0; i < 5; i++) {
    const siblingId = await smokePage.evaluate(async (workerFp) => {
      const smoke = (window as unknown as Window & {
        __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
      }).__smoke;
      return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
    }, stack.workerFp);
    await expect.poll(() => smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: { state(): { sessions: Record<string, unknown> } };
      }).__smoke;
      return id in smoke.state().sessions;
    }, siblingId)).toBe(true);
    await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
      smoke.navigate(`/s/${id}`);
    }, siblingId);
    await expect(smokePage.getByTestId(`tab-${siblingId}`)).toHaveAttribute("data-active", "true");
    siblings.push(siblingId);
  }
  for (const id of [sessionId, ...siblings]) {
    await expect(smokePage.getByTestId(`tab-${id}`)).toBeAttached();
  }

  // Let A's deferred withdraw complete, then prove it receives no cell frames
  // while another 1,000 markers are generated.
  await smokePage.waitForTimeout(1200);
  const frameCountBefore = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { cellFrameCount(sessionId: string): number } }).__smoke;
    return smoke.cellFrameCount(id);
  }, sessionId);
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & { __smoke: { input(sessionId: string, text: string): Promise<void> } }).__smoke;
    await smoke.input(id, "seq -f 'CELLLINE-%g' 8001 9000\r");
  }, sessionId);
  await smokePage.waitForTimeout(500);
  const frameCountAfter = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { cellFrameCount(sessionId: string): number } }).__smoke;
    return smoke.cellFrameCount(id);
  }, sessionId);
  expect(frameCountAfter).toBe(frameCountBefore);

  // A bottom-following stale viewer must reclaim only the 250-row tail.
  await smokePage.getByTestId(`tab-${sessionId}`).click();
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): { rowCount: number; atBottom: boolean };
        markerScan(sessionId: string, prefix: string): { max: number };
      };
    }).__smoke;
    const probe = smoke.renderProbe(id);
    return {
      markerMax: smoke.markerScan(id, "CELLLINE-").max,
      atBottom: probe.atBottom,
      boundedRows: probe.rowCount < 500,
    };
  }, sessionId), { timeout: 1000, intervals: [50] }).toEqual({
    markerMax: 9000,
    atBottom: true,
    boundedRows: true,
  });

  const revealed = await geometry();
  const revealRowSamples: number[] = [];
  for (let i = 0; i < 5; i++) {
    await smokePage.waitForTimeout(300);
    revealRowSamples.push((await geometry()).rowCount);
  }
  expect(revealRowSamples).toEqual(Array(5).fill(revealed.rowCount));

  // Move ten rows into the painted tail. The resulting scroll event is the
  // demand signal that may materialize older rows.
  await smokePage.evaluate(({ id, rowH }) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const container = pane?.querySelector(".wterm") as HTMLElement | null;
    const scrollback = container?.querySelector(".cell-scrollback") as HTMLElement | null;
    if (container && scrollback) container.scrollTop = scrollback.offsetTop + Math.round(10 * rowH);
  }, { id: sessionId, rowH: revealed.rowH });
  const parked = await geometry();
  expect(parked.reader).toMatch(/^CELLLINE-\d+$/);
  await expect.poll(async () => (await geometry()).rowCount, { timeout: 60_000 })
    .toBeGreaterThan(revealed.rowCount);

  // Wait for the demand-driven drain to quiesce.
  let last = -1;
  let settled = 0;
  await expect.poll(async () => {
    const current = await geometry();
    settled = current.rowCount === last ? settled + 1 : 0;
    last = current.rowCount;
    return settled;
  }, { timeout: 60_000, intervals: [250] }).toBeGreaterThanOrEqual(4);

  const drained = await geometry();
  expect(drained.rowCount).toBeGreaterThan(revealed.rowCount);
  expect(Math.abs(drained.scrollHeight - parked.scrollHeight)).toBeLessThanOrEqual(Math.ceil(drained.rowH));
  expect(drained.reader).toBe(parked.reader);
  expect(Math.abs(drained.scrollTop - parked.scrollTop)).toBeLessThanOrEqual(Math.ceil(drained.rowH));
  const scan = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { markerScan(sessionId: string, prefix: string): { duplicated: number[]; outOfOrder: number } };
    }).__smoke;
    return smoke.markerScan(id, "CELLLINE-");
  }, sessionId);
  expect(scan).toMatchObject({ duplicated: [], outOfOrder: 0 });
});
