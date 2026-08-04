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

  // Leading control: `+` opens the attachment sheet; typing swaps it to discard.
  await mobileSmokePage.getByTestId("terminal-chat-toggle").click();
  const lead = mobileSmokePage.getByTestId("chat-lead");
  await expect(lead).toHaveAttribute("data-mode", "add");
  await lead.click();
  await expect(mobileSmokePage.getByTestId("chat-add-photo")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("chat-add-gallery")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("chat-add-file")).toBeVisible();
  await mobileSmokePage.getByTestId("chat-input").fill("y");
  await expect(mobileSmokePage.getByTestId("chat-add-menu")).toHaveCount(0);
  await expect(lead).toHaveAttribute("data-mode", "clear");
  await lead.click();
  await expect(mobileSmokePage.getByTestId("chat-box")).toHaveCount(0);

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

  // A bottom-following stale viewer reclaims a BRIDGED tail: the claim carries
  // its held boundary (held_scrollback_total), the worker sizes the snapshot
  // back to it (cap SB_SNAPSHOT_MAX_CATCHUP_ROWS=2000) and mergeFullFrame
  // EXTENDS painted history — bounded, never a wipe to a 250-row tail that the
  // reader re-pulls 250 rows per round trip.
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
      // ≤ catch-up cap (2000) + viewport + slack — a collapse back to the
      // 250-row tail OR an unbounded full-history paint both fail this.
      boundedRows: probe.rowCount > 500 && probe.rowCount < 2300,
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

// Returning to an already-open pane must be INSTANT: no Sync-socket re-dial and
// no claim snapshot. Both legs assert the absence of work, which is why they
// read syncWsGeneration() (socket dial count) and cellFullFrameCount() (worker
// claim snapshots) rather than timing anything.
test("returning to a streaming pane costs no re-dial and no snapshot", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop deck/visibility contract");
  const spawn = (folder: string) => smokePage.evaluate(async ({ workerFp, dir }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, dir)).session_id;
  }, { workerFp: stack.workerFp, dir: folder });

  const sessionA = await spawn("/tmp");
  // Probe every quantity in ONE evaluate so a reveal can be inspected without
  // polling — polling would hide the very round trip under test.
  const probe = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): { atBottom: boolean };
        markerScan(sessionId: string, prefix: string): { max: number; duplicated: number[]; outOfOrder: number };
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
  // Let the burst quiesce: a frame in flight during a claim legitimately costs
  // one redundant snapshot, and this test asserts an exact count.
  await smokePage.waitForTimeout(1000);

  // ── leg 1: deck tab switch away and back ──────────────────────────────────
  const sessionB = await spawn("/tmp");
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { state(): { sessions: Record<string, unknown> } } }).__smoke;
    return id in smoke.state().sessions;
  }, sessionB)).toBe(true);
  const beforeSwitch = await probe();
  expect(beforeSwitch.markerMax).toBe(8000);

  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionB);
  await expect(smokePage.getByTestId(`tab-${sessionB}`)).toHaveAttribute("data-active", "true");
  await smokePage.waitForTimeout(1000); // A parks into the background-stream set
  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionA);
  await expect(smokePage.getByTestId(`tab-${sessionA}`)).toHaveAttribute("data-active", "true");

  // A never stopped streaming, so its reveal claim carries the worker's own last
  // emitted seq: the bottom is already painted and no full frame is emitted.
  const afterSwitch = await probe();
  expect(afterSwitch).toMatchObject({
    atBottom: true,
    markerMax: 8000,
    duplicated: [],
    outOfOrder: 0,
    fullFrames: beforeSwitch.fullFrames,
    wsGeneration: beforeSwitch.wsGeneration,
  });

  // ── leg 2: browser tab hidden, output while away, then refocus ────────────
  // setForceVisible dispatches a synthetic visibilitychange (lib/pageVisible.ts)
  // so the pane's handler AND sync-bootstrap's refocus handler run exactly as in
  // a real Chrome tab switch.
  const beforeHide = await probe();
  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke;
    smoke.forceVisible(false);
  });
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & { __smoke: { input(sessionId: string, text: string): Promise<void> } }).__smoke;
    await smoke.input(id, "for i in $(seq 8001 8200); do echo CELLLINE-$i; sleep 0.01; done\r");
  }, sessionA);
  // The hidden pane keeps its 0×0 BACKGROUND claim, so deltas keep painting
  // while the tab is away — wait for the LAST line before returning so the
  // reveal claim can't race a frame in flight.
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { markerScan(sessionId: string, prefix: string): { max: number } } }).__smoke;
    return smoke.markerScan(id, "CELLLINE-").max;
  }, sessionA), { timeout: 60_000, intervals: [250] }).toBe(8200);
  await smokePage.waitForTimeout(500);

  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke;
    smoke.forceVisible(true);
  });
  const afterShow = await probe();
  expect(afterShow).toMatchObject({
    atBottom: true,
    markerMax: 8200,
    duplicated: [],
    outOfOrder: 0,
    fullFrames: beforeHide.fullFrames,   // the pane never fell behind
    wsGeneration: beforeHide.wsGeneration, // the Sync socket survived the hide
  });
});

// The user's literal complaint, measured: switching to a stale deep-history
// pane must show the newest content at the literal bottom IMMEDIATELY. Every
// painted sample during the reveal is at the bottom (never mid-history, never
// inside the spacer watching a top-down crawl), the fresh tail is visible
// within 1500 ms, painted history is not collapsed (merge path), and nothing
// drains phantom rows after settle. Prior fixes "passed" because nothing
// asserted what the reader SEES at first paint.
test("deck switch to a stale deep-history pane lands at the live bottom instantly", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop scroll-geometry contract");
  test.setTimeout(240_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'SWL-%g' 1 8000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 60_000 }).toContain("SWL-8000");

  // Five same-folder siblings visited in sequence push A past the 4-slot
  // background-stream LRU: parked AND withdrawn, the stalest possible pane.
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
  await smokePage.waitForTimeout(1200); // A's deferred withdraw completes

  // 300 fresh rows land while A is withdrawn (≤2000 behind → merge path), and
  // A provably receives none of them.
  const frameCountBefore = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { cellFrameCount(sessionId: string): number } }).__smoke;
    return smoke.cellFrameCount(id);
  }, sessionId);
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & { __smoke: { input(sessionId: string, text: string): Promise<void> } }).__smoke;
    await smoke.input(id, "seq -f 'FRESH-%g' 1 300\r");
  }, sessionId);
  await smokePage.waitForTimeout(500);
  expect(await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { cellFrameCount(sessionId: string): number } }).__smoke;
    return smoke.cellFrameCount(id);
  }, sessionId)).toBe(frameCountBefore);

  const probe = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { renderProbe(sessionId: string): { rowCount: number; atBottom: boolean } };
    }).__smoke;
    return smoke.renderProbe(id);
  }, sessionId);
  const preSwitch = await probe();
  expect(preSwitch.rowCount).toBeGreaterThan(1500); // deep painted history held while parked

  // 50 ms position sampler across the whole reveal: at EVERY sample where any
  // .cell-row is painted, the reader is at the literal bottom (±2 px). A
  // transient trip through mid-history or the blank spacer fails the run.
  const startSampler = () => smokePage.evaluate((id) => {
    const w = window as unknown as Window & { __swlSamples?: Array<{ painted: number; top: number; height: number; client: number }>; __swlTimer?: number };
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slot?.querySelector(".wterm") as HTMLElement | null;
    const samples: Array<{ painted: number; top: number; height: number; client: number }> = [];
    w.__swlSamples = samples;
    w.__swlTimer = window.setInterval(() => {
      if (!c) return;
      samples.push({
        painted: c.querySelectorAll(".cell-row").length,
        top: c.scrollTop, height: c.scrollHeight, client: c.clientHeight,
      });
    }, 50);
  }, sessionId);
  const stopSampler = () => smokePage.evaluate(() => {
    const w = window as unknown as Window & { __swlSamples?: Array<{ painted: number; top: number; height: number; client: number }>; __swlTimer?: number };
    if (w.__swlTimer !== undefined) window.clearInterval(w.__swlTimer);
    return w.__swlSamples ?? [];
  });
  // Bottommost FRESH marker actually visible inside the container's box —
  // "the newest content is at the literal bottom", by geometry.
  const maxVisibleMarker = (prefix: string) => smokePage.evaluate(({ id, prefix }) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slot?.querySelector(".wterm") as HTMLElement | null;
    if (!c) return -1;
    const box = c.getBoundingClientRect();
    let max = -1;
    const re = new RegExp(prefix + "(\\d+)");
    for (const row of c.querySelectorAll(".cell-row")) {
      const r = row.getBoundingClientRect();
      if (r.bottom <= box.top + 1 || r.top >= box.bottom - 1) continue;
      const m = (row.textContent ?? "").match(re);
      if (m) max = Math.max(max, Number(m[1]));
    }
    return max;
  }, { id: sessionId, prefix });

  await startSampler();
  await smokePage.getByTestId(`tab-${sessionId}`).click();
  // (2) the fresh tail is VISIBLE at the bottom within 1500 ms.
  await expect.poll(() => maxVisibleMarker("FRESH-"), { timeout: 1_500, intervals: [50] }).toBe(300);
  await smokePage.waitForTimeout(2_000); // (1) keep sampling through settle
  const samples = await stopSampler();
  expect(samples.length).toBeGreaterThan(10);
  const offBottom = samples.filter((s) => s.painted > 0 && s.top < s.height - s.client - 2);
  expect(offBottom).toEqual([]);

  // (3) history NOT collapsed: painted depth stays in the merged band. Block-
  // granular eviction may trim up to one 250-row block below the pre-switch
  // count; a wipe to the 250-row tail is an order of magnitude below this.
  const settled = await probe();
  expect(settled.rowCount).toBeGreaterThan(1500);
  expect(settled.rowCount).toBeGreaterThanOrEqual(preSwitch.rowCount - 300);
  expect(settled.atBottom).toBe(true);

  // (4) no phantom drain at the bottom: painted rows stay FLAT after settle.
  const flat: number[] = [];
  for (let i = 0; i < 5; i++) {
    await smokePage.waitForTimeout(300);
    flat.push((await probe()).rowCount);
  }
  expect(flat).toEqual(Array(5).fill(settled.rowCount));
  const scan = await smokePage.evaluate((sid) => {
    const smoke = (window as unknown as Window & {
      __smoke: { markerScan(sessionId: string, prefix: string): { duplicated: number[]; outOfOrder: number; missing: number } };
    }).__smoke;
    return smoke.markerScan(sid, "SWL-");
  }, sessionId);
  expect(scan).toMatchObject({ duplicated: [], outOfOrder: 0, missing: 0 });

  // ── Variant: parked >2000 behind → epoch collapse is allowed, the bottom is
  // mandatory. The held window has no image in the new epoch; the reveal must
  // land on the present as a bounded ≤2000-row catch-up tail. Re-visit all
  // five siblings so A falls past the 4-slot background LRU again — a single
  // navigation would leave A background-streaming (never stale).
  for (const sid of siblings) {
    await smokePage.evaluate((s) => {
      const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
      smoke.navigate(`/s/${s}`);
    }, sid);
    await expect(smokePage.getByTestId(`tab-${sid}`)).toHaveAttribute("data-active", "true");
  }
  await smokePage.waitForTimeout(1200); // withdraw completes again
  const frameCountBefore2 = await smokePage.evaluate((sid) => {
    const smoke = (window as unknown as Window & { __smoke: { cellFrameCount(sessionId: string): number } }).__smoke;
    return smoke.cellFrameCount(sid);
  }, sessionId);
  await smokePage.evaluate(async (sid) => {
    const smoke = (window as unknown as Window & { __smoke: { input(sessionId: string, text: string): Promise<void> } }).__smoke;
    await smoke.input(sid, "seq -f 'FRESH2-%g' 1 3000\r");
  }, sessionId);
  await smokePage.waitForTimeout(3_000); // worker finishes generating unwatched
  expect(await smokePage.evaluate((sid) => {
    const smoke = (window as unknown as Window & { __smoke: { cellFrameCount(sessionId: string): number } }).__smoke;
    return smoke.cellFrameCount(sid);
  }, sessionId)).toBe(frameCountBefore2); // A provably withdrawn → >2000 stale

  await startSampler();
  await smokePage.getByTestId(`tab-${sessionId}`).click();
  await expect.poll(() => maxVisibleMarker("FRESH2-"), { timeout: 1_500, intervals: [50] }).toBe(3000);
  await smokePage.waitForTimeout(2_000);
  const samples2 = await stopSampler();
  const offBottom2 = samples2.filter((s) => s.painted > 0 && s.top < s.height - s.client - 2);
  expect(offBottom2).toEqual([]);
  const collapsed = await probe();
  expect(collapsed.atBottom).toBe(true);
  // Rebuilt depth is the bounded catch-up tail: ≤ 2000 + viewport + slack.
  expect(collapsed.rowCount).toBeLessThan(2300);
});

// Bottom-follow must survive geometry changes that happen while a pane is
// parked: the box shrinks (window resize / keyboard inset / divider drag),
// nothing re-samples the bottom, and pre-noteBoxResize the pane revealed
// off-bottom with live output landing below the fold — permanently.
test("a pane revealed after the window shrank is still at the bottom", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop scroll-geometry contract");
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'SHRK-%g' 1 600");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 30_000 }).toContain("SHRK-600");

  // Park A behind a sibling, then shrink the window UNDER the parked pane.
  const siblingId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, siblingId);
  await expect(smokePage.getByTestId(`tab-${siblingId}`)).toHaveAttribute("data-active", "true");
  const vp = smokePage.viewportSize()!;
  // −100 px keeps the short side ≥ 600 (windowSizeClass compact boundary keys
  // on min(w,h)): the pane must SHRINK, not flip the whole app to mobile UI.
  await smokePage.setViewportSize({ width: vp.width, height: vp.height - 100 });
  await smokePage.waitForTimeout(400); // park restyle + ResizeObserver tick

  // Reveal: the FIRST sample with painted rows is already at the bottom.
  await smokePage.evaluate((id) => {
    const w = window as unknown as Window & { __shrkSamples?: Array<{ painted: number; top: number; height: number; client: number }>; __shrkTimer?: number };
    const slotEl = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slotEl?.querySelector(".wterm") as HTMLElement | null;
    const samples: Array<{ painted: number; top: number; height: number; client: number }> = [];
    w.__shrkSamples = samples;
    w.__shrkTimer = window.setInterval(() => {
      if (!c) return;
      samples.push({
        painted: c.querySelectorAll(".cell-row").length,
        top: c.scrollTop, height: c.scrollHeight, client: c.clientHeight,
      });
    }, 50);
  }, sessionId);
  await smokePage.getByTestId(`tab-${sessionId}`).click();
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { renderProbe(sessionId: string): { atBottom: boolean; rowCount: number } };
    }).__smoke;
    const p = smoke.renderProbe(id);
    return p.rowCount > 0 && p.atBottom;
  }, sessionId), { timeout: 1_500, intervals: [50] }).toBe(true);
  await smokePage.waitForTimeout(1_000);
  const samples = await smokePage.evaluate(() => {
    const w = window as unknown as Window & { __shrkSamples?: Array<{ painted: number; top: number; height: number; client: number }>; __shrkTimer?: number };
    if (w.__shrkTimer !== undefined) window.clearInterval(w.__shrkTimer);
    return w.__shrkSamples ?? [];
  });
  expect(samples.length).toBeGreaterThan(5);
  const offBottom = samples.filter((s) => s.painted > 0 && s.top < s.height - s.client - 2);
  expect(offBottom).toEqual([]);
  // The newest marker is visible at the bottom of the SHRUNKEN box.
  const lastVisible = await smokePage.evaluate((id) => {
    const slotEl = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slotEl?.querySelector(".wterm") as HTMLElement | null;
    if (!c) return -1;
    const box = c.getBoundingClientRect();
    let max = -1;
    for (const row of c.querySelectorAll(".cell-row")) {
      const r = row.getBoundingClientRect();
      if (r.bottom <= box.top + 1 || r.top >= box.bottom - 1) continue;
      const m = (row.textContent ?? "").match(/SHRK-(\d+)/);
      if (m) max = Math.max(max, Number(m[1]));
    }
    return max;
  }, sessionId);
  expect(lastVisible).toBe(600);
});

// A /file/… visit must NOT tear down the terminal deck: MainPane's screens
// share ONE route definition (App.tsx) and the deck host only flips
// visibility (MainPane.tsx). Separate <Route> entries remounted MainPane on
// every /s ↔ /file crossing — every renderer died, warmSessionIds reset, and
// the return was a cold mount + claim storm. Node identity + a flat full-frame
// count are the remount detectors; nothing else in this suite navigates /file,
// which is exactly why the regression slipped past green runs.
test("a /file round-trip keeps the deck warm and costs no snapshot", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop deck-persistence contract");
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'FRT-%g' 1 300");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 30_000 }).toContain("FRT-300");

  const baseline = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { cellFullFrameCount(sessionId: string): number; syncWsGeneration(): number };
    }).__smoke;
    const deck = document.querySelector('[data-testid="terminal-deck"]') as (HTMLElement & { __persistCanary?: string }) | null;
    if (deck) deck.__persistCanary = "alive";
    return { fullFrames: smoke.cellFullFrameCount(id), wsGeneration: smoke.syncWsGeneration(), canarySet: !!deck };
  }, sessionId);
  expect(baseline.canarySet).toBe(true);

  // Into the file viewer: deck stays in the DOM, merely visibility-hidden.
  await smokePage.evaluate((fp) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/file/${fp}/tmp/roost-frt-missing.txt`);
  }, stack.workerFp);
  await expect.poll(() => smokePage.evaluate(() => {
    const deck = document.querySelector('[data-testid="terminal-deck"]') as (HTMLElement & { __persistCanary?: string }) | null;
    if (!deck?.parentElement) return null;
    return { canary: deck.__persistCanary ?? null, hostVis: getComputedStyle(deck.parentElement).visibility };
  })).toEqual({ canary: "alive", hostVis: "hidden" });

  // And back: the SAME deck node, zero full frames, zero re-dials — the
  // return is a pure visibility flip, and the pane is still at the bottom.
  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionId);
  await expect(slot).toBeVisible();
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        cellFullFrameCount(sessionId: string): number;
        syncWsGeneration(): number;
        renderProbe(sessionId: string): { atBottom: boolean; rowCount: number };
        markerScan(sessionId: string, prefix: string): { max: number; duplicated: number[]; outOfOrder: number };
      };
    }).__smoke;
    const deck = document.querySelector('[data-testid="terminal-deck"]') as (HTMLElement & { __persistCanary?: string }) | null;
    if (!deck?.parentElement) return null;
    const scan = smoke.markerScan(id, "FRT-");
    return {
      canary: deck.__persistCanary ?? null,
      hostVis: getComputedStyle(deck.parentElement).visibility,
      fullFrames: smoke.cellFullFrameCount(id),
      wsGeneration: smoke.syncWsGeneration(),
      atBottom: smoke.renderProbe(id).atBottom,
      markerMax: scan.max,
      duplicated: scan.duplicated,
      outOfOrder: scan.outOfOrder,
    };
  }, sessionId)).toEqual({
    canary: "alive",
    hostVis: "visible",
    fullFrames: baseline.fullFrames,
    wsGeneration: baseline.wsGeneration,
    atBottom: true,
    markerMax: 300,
    duplicated: [],
    outOfOrder: 0,
  });
});
