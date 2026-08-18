import { test, expect } from "./fixtures.ts";
import { encodePtyFixtureCommand, PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  spawnPtyFixtureSession,
  navigateToSmokeSession,
  inputSmokeTerminal,
} from "./terminal-helpers.ts";

// A fresh deep-session attach starts with the current viewport and a truthful
// spacer only. History stays off the network and out of the DOM until demand.
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
        markerScan(sessionId: string, prefix: string): {
          max: number;
          duplicated: number[];
          missing: number;
          outOfOrder: number;
        };
        lastFullFrameSbRows(sessionId: string): number;
        scrollbackBackfillRequestCount(sessionId: string): number;
      };
    }).__smoke;
    return {
      ...smoke.renderProbe(id),
      markerMax: smoke.markerScan(id, "CELLLINE-").max,
      snapshotSbRows: smoke.lastFullFrameSbRows(id),
      historyRequests: smoke.scrollbackBackfillRequestCount(id),
    };
  }, sessionId);

  const attached = await probe();
  expect(attached).toMatchObject({
    mode: "cell",
    markerMax: 8000,
    atBottom: true,
    snapshotSbRows: 0,
    historyRequests: 0,
  });
  expect(attached.rowCount).toBeLessThan(100);

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
        markerScan(sessionId: string, prefix: string): {
          duplicated: number[];
          missing: number;
          outOfOrder: number;
        };
      };
    }).__smoke;
    return smoke.markerScan(id, "CELLLINE-");
  }, sessionId);
  expect(demanded).toMatchObject({ duplicated: [], missing: 0, outOfOrder: 0 });
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
  const linkText = `r-${suffix}`;
  const firstUri = `https://osc8.test/${suffix}/one`;
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

  const live = await paintedLinks();
  expect(live.map((link) => link.href)).toEqual([firstUri, secondUri]);
  // Distinct run identity is what keeps two identically-styled, identically-
  // texted links from coalescing into one span and losing a URI.
  expect(new Set(live.map((link) => link.key)).size).toBe(2);
  // The inferred scan is asynchronous (idle/rAF), unlike the painted anchors,
  // which exist the instant the row paints.
  await expect.poll(inferredUrls, { timeout: 15_000, intervals: [100] }).toEqual([inferredUrl]);
  const liveFiles = await fileLinks();
  expect(liveFiles.map((link) => link.text)).toEqual([filePath]);
  // The internal file route, not a browser navigation — Roost opens it itself.
  expect(liveFiles[0].href.startsWith("/file/")).toBe(true);

  // Push the link row deep into retained history, then reload: the authoritative
  // snapshot paints only the live tail plus a spacer, so the anchors leave the
  // DOM entirely and can only come back through a history page.
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "FLOOD", prefix: `OSC8-FILL-${suffix}-`, count: 200 }),
  );
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 30_000);
  }, { id: sessionId, marker: `OSC8-FILL-${suffix}-200` });

  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await smokePage.waitForFunction(() =>
    typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  await smokePage.waitForFunction((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    return !!pane?.querySelector(".cell-sb-spacer") && !!pane.querySelector(".cell-row");
  }, sessionId);
  expect(await paintedLinks()).toEqual([]);
  expect(await inferredUrls()).toEqual([]);
  expect(await fileLinks()).toEqual([]);

  const beforeHistory = await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.scrollbackBackfillRequestCount(id);
  }, sessionId);
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
  }, { id: sessionId, previous: beforeHistory });

  await expect.poll(
    async () => (await paintedLinks()).map((link) => link.href),
    { timeout: 30_000, intervals: [100] },
  ).toEqual([firstUri, secondUri]);
  expect(new Set((await paintedLinks()).map((link) => link.key)).size).toBe(2);
  await expect.poll(inferredUrls, { timeout: 15_000, intervals: [100] }).toEqual([inferredUrl]);
  expect((await fileLinks()).map((link) => link.text)).toEqual([filePath]);
});
