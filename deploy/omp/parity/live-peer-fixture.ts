// Owns Playwright Chromium for the local live service's answering media peer.
// live-fixture-service supplies only its owned loopback origin and prepared capture WAV.
// Browser APIs are real; all page HTTP and WebSocket destinations are closed by default.

import { chromium, type Browser, type BrowserContext, type Page } from "playwright";
import { installLivePeerRuntime, type LivePeerMetrics, type LivePeerWindow } from "./live-peer-runtime";

export interface LiveFixturePeer {
  answer(offer: string): Promise<string>;
  close(): Promise<LivePeerMetrics>;
  dispose(): Promise<void>;
}

export async function startLiveFixturePeer(
  origin: string, wav: string, observe: (value: Record<string, unknown>) => void,
): Promise<LiveFixturePeer> {
  let browser: Browser | undefined;
  let context: BrowserContext | undefined;
  let page: Page | undefined;
  let disposal: Promise<void> | undefined;
  const boundedPeerOperation = async <Value>(operation: Promise<Value>, milliseconds: number): Promise<Value> => {
    const expiry = Promise.withResolvers<never>();
    const timeout = setTimeout(() => expiry.reject(new Error("Live fixture peer operation timed out")), milliseconds);
    try { return await Promise.race([operation, expiry.promise]); }
    finally { clearTimeout(timeout); }
  };
  const dispose = (): Promise<void> => {
    disposal ??= (async () => {
      const failures: unknown[] = [];
      try { await context?.close(); } catch (error) { failures.push(error); }
      try { await browser?.close(); } catch (error) { failures.push(error); }
      if (failures.length) throw new AggregateError(failures, "Failed to close owned live fixture browser");
    })();
    return disposal;
  };
  try {
    browser = await chromium.launch({
      headless: true, timeout: 15_000,
      args: [
        "--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream",
        `--use-file-for-fake-audio-capture=${wav}`, "--mute-audio",
        "--disable-background-networking", "--disable-component-update",
        "--disable-domain-reliability", "--disable-sync", "--no-first-run",
        "--force-webrtc-ip-handling-policy=default_public_interface_only",
      ],
    });
    context = await browser.newContext({ serviceWorkers: "block", permissions: [] });
    context.setDefaultTimeout(12_000);
    await context.grantPermissions(["microphone"], { origin });
    await context.route("**/*", async route => {
      const request = route.request();
      if (request.url() === `${origin}/peer` && request.method() === "GET" && request.isNavigationRequest()) {
        await route.continue();
      } else {
        observe({ kind: "failure", message: "Live fixture browser refused unexpected HTTP destination", url: request.url() });
        await route.abort("blockedbyclient");
      }
    });
    await context.routeWebSocket("**/*", socket => {
      observe({ kind: "failure", message: "Live fixture browser refused unexpected WebSocket", url: socket.url() });
      socket.close({ code: 1008, reason: "No fixture browser WebSocket route" });
    });
    await context.exposeBinding("__ompParityLivePeerEvent", (_source, value: unknown) => {
      if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid live peer observation");
      observe(value as Record<string, unknown>);
    });
    page = await context.newPage();
    page.on("pageerror", error => observe({ kind: "failure", message: error.message }));
    await page.goto(`${origin}/peer`, { waitUntil: "domcontentloaded", timeout: 12_000 });
    await page.evaluate(installLivePeerRuntime);
    const ownedPage = page;
    return {
      answer: offer => boundedPeerOperation(ownedPage.evaluate(offer => (window as LivePeerWindow).__ompParityLivePeer.answer(offer), offer), 12_000),
      close: () => boundedPeerOperation(ownedPage.evaluate(() => (window as LivePeerWindow).__ompParityLivePeer.close()), 5_000),
      dispose,
    };
  } catch (error) {
    try { await dispose(); }
    catch (cleanupError) { throw new AggregateError([error, cleanupError], "Live fixture browser startup and cleanup failed"); }
    throw error;
  }
}
