import { test, expect } from "./fixtures.ts";
import { SB_RENEWAL_HISTORY_ROWS } from "../../apps/shared/src/cell/types.ts";
import { TERMINAL_MAX_ROWS } from "../../apps/shared/src/viewport.ts";
import { encodePtyFixtureCommand, PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import type { RecoveryMarkerScan, RecoverySmokeApi } from "./terminal-smoke-api.ts";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import {
  spawnPtyFixtureSession,
  navigateToSmokeSession,
  inputSmokeTerminal,
} from "./terminal-helpers.ts";

// A fresh deep-session renewal starts at the live tail with a bounded retained
// history window. Older history stays off the network until explicit demand.
test("deep-history attach/reveal paints the live tail until history is requested", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop scroll-geometry contract");
  test.setTimeout(120_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'CELLLINE-%g' 1 8000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 60_000 }).toContain("CELLLINE-8000");

  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await smokePage.waitForFunction(() =>
    typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  await smokePage.waitForFunction((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    return !!pane?.querySelector(".cell-sb-spacer") && !!pane.querySelector(".cell-row");
  }, sessionId);

  const probe = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): {
          mode: "cell" | "byte" | "none";
          rowCount: number;
          atBottom: boolean;
        };
        markerScan(sessionId: string, prefix: string): RecoveryMarkerScan;
        lastFullFrameSbRows(sessionId: string): number;
        scrollbackBackfillRequestCount(sessionId: string): number;
      };
    }).__smoke;
    const scan = smoke.markerScan(id, "CELLLINE-");
    return {
      ...smoke.renderProbe(id),
      scan,
      snapshotSbRows: smoke.lastFullFrameSbRows(id),
      historyRequests: smoke.scrollbackBackfillRequestCount(id),
    };
  }, sessionId);

  const attached = await probe();
  expect(attached).toMatchObject({
    mode: "cell",
    atBottom: true,
    historyRequests: 0,
    scan: {
      max: 8000,
      duplicated: [],
      missing: 0,
      outOfOrder: 0,
    },
  });
  expect(attached.snapshotSbRows).toBeGreaterThan(0);
  expect(attached.snapshotSbRows).toBeLessThanOrEqual(SB_RENEWAL_HISTORY_ROWS);
  expect(attached.rowCount).toBeGreaterThan(0);
  const paintedViewportRows = attached.rowCount - attached.snapshotSbRows;
  expect(paintedViewportRows).toBeGreaterThan(0);
  expect(paintedViewportRows).toBeLessThanOrEqual(TERMINAL_MAX_ROWS);
  expect(attached.rowCount).toBeLessThanOrEqual(
    SB_RENEWAL_HISTORY_ROWS + TERMINAL_MAX_ROWS,
  );

  const idleSamples = await smokePage.evaluate(async ({ id, rowCount, requests }) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): { rowCount: number; atBottom: boolean };
        scrollbackBackfillRequestCount(sessionId: string): number;
      };
    }).__smoke;
    const samples: Array<{ rowCount: number; requests: number; atBottom: boolean }> = [];
    for (let frame = 0; frame < 8; frame++) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      samples.push({
        rowCount: smoke.renderProbe(id).rowCount,
        requests: smoke.scrollbackBackfillRequestCount(id),
        atBottom: smoke.renderProbe(id).atBottom,
      });
    }
    return { samples, expected: { rowCount, requests, atBottom: true } };
  }, { id: sessionId, rowCount: attached.rowCount, requests: attached.historyRequests });
  expect(idleSamples.samples).toEqual(Array(8).fill(idleSamples.expected));

  await smokePage.evaluate((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const container = pane?.querySelector(".wterm") as HTMLElement | null;
    if (!container) return;
    container.scrollTop = 0;
    container.dispatchEvent(new Event("scroll"));
  }, sessionId);
  await smokePage.waitForFunction(({ id, previous }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { scrollbackBackfillRequestCount(sessionId: string): number };
    }).__smoke;
    return smoke.scrollbackBackfillRequestCount(id) > previous;
  }, { id: sessionId, previous: attached.historyRequests });
  await smokePage.waitForFunction(({ id, previous }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { renderProbe(sessionId: string): { rowCount: number } };
    }).__smoke;
    return smoke.renderProbe(id).rowCount > previous;
  }, { id: sessionId, previous: attached.rowCount });

  const demanded = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        markerScan(sessionId: string, prefix: string): RecoveryMarkerScan;
      };
    }).__smoke;
    return smoke.markerScan(id, "CELLLINE-");
  }, sessionId);
  expect(demanded).toMatchObject({
    max: 8000,
    duplicated: [],
    missing: 0,
    outOfOrder: 0,
  });
  expect(demanded.total).toBeGreaterThan(attached.scan.total);
});

// OSC 8 hyperlinks are CORE-AUTHORED per cell — never derived from the byte
// stream, never matched by TEXT. This is the scenario the old text→URI matcher
// could not express: two links with IDENTICAL visible text and DIFFERENT URIs on
// one row. A text→URI map holds one entry per text, so the second link either
// overwrote the first or was dropped, and BOTH anchors then pointed at the same
// place (and the same text appearing anywhere else became a link too). Per-cell
// identity keeps them apart. The run then has to survive a scrollback backfill
// round-trip, which serves history rows from the live core's own scrollback —
// a second, independent path to the same per-cell link data.
test("identical link text with different URIs keeps both, through a backfill round-trip", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop cell paint contract");
  test.setTimeout(120_000);
  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
  await navigateToSmokeSession(smokePage, sessionId);
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 30_000);
  }, { id: sessionId, marker: PTY_FIXTURE_READY });

  // Every VISIBLE token is kept short on purpose: the whole emit is ONE row, and
  // a row that reaches the pane's grid width soft-wraps — which would split
  // `linkMarker` across two rows, and findPaintedMarker matches per row. 55
  // columns fits any plausible desktop pane.
  // Leading 'z' also on purpose: a pure-hex suffix is a valid bare commit SHA,
  // which the INFERRED GitHub-ref pass would linkify on its own and muddy the
  // count (the leading word character defeats that regex's lookbehind).
  const suffix = `z${crypto.randomUUID().replaceAll("-", "").slice(0, 7)}`;
  const linkedFileMarker = `terminal-link-line-9-${suffix}`;
  await mkdir(join(fixtureWorker.home, "s"), { recursive: true });
  const linkedFileLines = Array.from({ length: 12 }, (_, index) => `fixture line ${index + 1}`);
  linkedFileLines[8] = linkedFileMarker;
  await writeFile(join(fixtureWorker.home, "s", "f.ts"), linkedFileLines.join("\n"), "utf8");
  const linkText = `r-${suffix}`;
  const firstUri = `${new URL(smokePage.url()).origin}/favicon.ico?terminal-link=${suffix}`;
  const secondUri = `https://osc8.test/${suffix}/two`;
  const hyperlink = (uri: string): string => `\u001b]8;;${uri}\u0007${linkText}\u001b]8;;\u0007`;
  const linkMarker = `O8-${suffix}`;
  // A plain URL and a resolvable file path on the SAME row. Producer links are
  // painted by the renderer while these two are found by the inferred scan, and
  // the authorities must coexist: the scan's "already handled" mark is a MARK,
  // not "this row has an anchor", or a painted link would suppress every regex
  // and file link sharing its row.
  const inferredUrl = `https://i.test/${suffix}`;
  const filePath = "s/f.ts:9";
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({
      op: "EMIT",
      text: `${hyperlink(firstUri)} ${hyperlink(secondUri)} ${inferredUrl} ${filePath} ${linkMarker}`,
    }),
  );
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 30_000);
  }, { id: sessionId, marker: linkMarker });

  const paintedLinks = (): Promise<Array<{ href: string | null; key: string | null }>> =>
    smokePage.evaluate(({ id, text }) => {
      const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
      return Array.from(slot?.querySelectorAll("a.wterm-link[data-link-key]") ?? [])
        .filter((anchor) => (anchor.textContent ?? "") === text)
        .map((anchor) => ({
          href: anchor.getAttribute("href"),
          key: anchor.getAttribute("data-link-key"),
        }));
    }, { id: sessionId, text: linkText });
  const inferredUrls = (): Promise<string[]> =>
    smokePage.evaluate((id) => {
      const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
      const selector = 'a.wterm-link:not([data-link-key]):not([data-kind="file"])';
      return Array.from(slot?.querySelectorAll(selector) ?? [])
        .map((anchor) => anchor.getAttribute("href") ?? "");
    }, sessionId);
  const fileLinks = (): Promise<Array<{ href: string; text: string }>> =>
    smokePage.evaluate((id) => {
      const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
      return Array.from(slot?.querySelectorAll('a.wterm-link[data-kind="file"]') ?? [])
        .map((anchor) => ({
          href: anchor.getAttribute("href") ?? "",
          text: anchor.textContent ?? "",
        }));
    }, sessionId);
  const expectLinksIntact = async (): Promise<void> => {
    await expect.poll(
      async () => (await paintedLinks()).map((link) => link.href),
      { timeout: 30_000, intervals: [100] },
    ).toEqual([firstUri, secondUri]);
    const producerLinks = await paintedLinks();
    // Distinct run identity is what keeps identically-styled, identically-
    // texted links from coalescing into one span and losing a URI.
    expect(new Set(producerLinks.map((link) => link.key)).size).toBe(2);
    await expect.poll(inferredUrls, { timeout: 15_000, intervals: [100] }).toEqual([inferredUrl]);
    await expect.poll(
      async () => (await fileLinks()).map((link) => link.text),
      { timeout: 15_000, intervals: [100] },
    ).toEqual([filePath]);
    const files = await fileLinks();
    // The internal file route, not a browser navigation — Roost opens it itself.
    expect(files[0]?.href.startsWith("/file/")).toBe(true);
  };

  await expectLinksIntact();
  // A real modifier-click must request the exact external target; worker files stay in-app.
  const linkModifier = await smokePage.evaluate((): "Meta" | "Control" =>
    /Mac|iPhone|iPad/i.test(navigator.platform) ? "Meta" : "Control");
  const producerAnchor = smokePage.locator('a.wterm-link[data-link-key]', { hasText: linkText }).first();
  const popupContext = smokePage.context();
  const pagesBeforeActivation = new Set(popupContext.pages());
  await popupContext.route(firstUri, (route) =>
    route.fulfill({ status: 200, contentType: "text/html", body: "<!doctype html>" }));
  try {
    await smokePage.keyboard.down(linkModifier);
    try {
      const [activationRequest] = await Promise.all([
        popupContext.waitForEvent("request", {
          predicate: (request) => request.url() === firstUri, timeout: 15_000,
        }),
        producerAnchor.click({ button: "left" }),
      ]);
      expect(activationRequest.url()).toBe(firstUri);
    } finally {
      await smokePage.keyboard.up(linkModifier).catch(() => undefined);
    }
  } finally {
    const extraPages = popupContext.pages().filter((page) => !pagesBeforeActivation.has(page));
    await Promise.all(extraPages.map((page) => page.close().catch(() => undefined)));
    await popupContext.unroute(firstUri).catch(() => undefined);
  }

  const fileAnchor = smokePage
    .locator('a.wterm-link[data-kind="file"]')
    .filter({ hasText: filePath });
  await smokePage.keyboard.down(linkModifier);
  try {
    await fileAnchor.click({ button: "left" });
  } finally {
    await smokePage.keyboard.up(linkModifier);
  }
  await expect(smokePage).toHaveURL(/\/file\/.+#L9$/);
  await expect(smokePage.getByTestId("file-viewer-sheet")).toBeVisible();
  await expect(smokePage.getByTestId("file-viewer-sheet-line-9")).toContainText(linkedFileMarker);
  await expect(smokePage.getByTestId("file-viewer-sheet-line-num-9")).toHaveAttribute("data-target", "true");
  await smokePage.goBack({ waitUntil: "domcontentloaded" });
  await expect(smokePage.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();

  // A same-grid renewal is allowed to carry a bounded history tail. Keep the
  // link close enough to the tail to prove that path preserves both producer
  // identities without issuing the demand-only history RPC.
  const retainedPrefix = `OSC8-FILL-${suffix}-`;
  const retainedCount = 200;
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "FLOOD", prefix: retainedPrefix, count: retainedCount }),
  );
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 30_000);
  }, { id: sessionId, marker: `${retainedPrefix}${retainedCount}` });

  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await smokePage.waitForFunction(() => {
    const smokeWindow = window as unknown as Window & { __smoke?: unknown };
    return typeof smokeWindow.__smoke === "object";
  });
  await smokePage.waitForFunction((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    return !!pane?.querySelector(".cell-sb-spacer") && !!pane.querySelector(".cell-row");
  }, sessionId);

  const renewed = await smokePage.evaluate(({ id, prefix }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    return {
      historyRequests: smoke.scrollbackBackfillRequestCount(id),
      snapshotSbRows: smoke.lastFullFrameSbRows(id),
      scan: smoke.markerScan(id, prefix),
    };
  }, { id: sessionId, prefix: retainedPrefix });
  expect(renewed.historyRequests).toBe(0);
  expect(renewed.snapshotSbRows).toBeGreaterThan(0);
  expect(renewed.snapshotSbRows).toBeLessThanOrEqual(SB_RENEWAL_HISTORY_ROWS);
  expect(renewed.scan.total).toBeGreaterThan(0);
  expect(renewed.scan).toMatchObject({
    max: retainedCount,
    duplicated: [],
    missing: 0,
    outOfOrder: 0,
  });
  await expectLinksIntact();

  // Advance beyond the renewal window while staying at the live bottom. The
  // original link row must leave the bounded DOM tail, but no paging is allowed
  // until the user actually asks for the older rows.
  const deepPrefix = `OSC8-DEEP-${suffix}-`;
  const deepCount = SB_RENEWAL_HISTORY_ROWS + 300;
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "FLOOD", prefix: deepPrefix, count: deepCount }),
  );
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 30_000);
  }, { id: sessionId, marker: `${deepPrefix}${deepCount}` });

  const beforeDemand = await smokePage.evaluate(({ id, prefix }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    return {
      atBottom: smoke.renderProbe(id).atBottom,
      historyRequests: smoke.scrollbackBackfillRequestCount(id),
      scan: smoke.markerScan(id, prefix),
    };
  }, { id: sessionId, prefix: deepPrefix });
  expect(beforeDemand.atBottom).toBe(true);
  expect(beforeDemand.historyRequests).toBe(renewed.historyRequests);
  expect(beforeDemand.scan.total).toBeGreaterThan(0);
  expect(beforeDemand.scan.min).toBeGreaterThan(1);
  expect(beforeDemand.scan).toMatchObject({
    max: deepCount,
    duplicated: [],
    missing: 0,
    outOfOrder: 0,
  });
  expect(await paintedLinks()).toEqual([]);
  expect(await inferredUrls()).toEqual([]);
  expect(await fileLinks()).toEqual([]);

  await smokePage.evaluate((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const container = pane?.querySelector(".wterm");
    if (!(container instanceof HTMLElement)) throw new Error("terminal has no scroll container");
    container.scrollTop = 0;
    container.dispatchEvent(new Event("scroll"));
  }, sessionId);
  await smokePage.waitForFunction(({ id, previous }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.scrollbackBackfillRequestCount(id) > previous;
  }, { id: sessionId, previous: beforeDemand.historyRequests });

  await expectLinksIntact();
  const demanded = await smokePage.evaluate(({ id, prefix }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    return smoke.markerScan(id, prefix);
  }, { id: sessionId, prefix: deepPrefix });
  expect(demanded).toMatchObject({
    total: deepCount,
    unique: deepCount,
    min: 1,
    max: deepCount,
    duplicated: [],
    missing: 0,
    outOfOrder: 0,
  });
});
