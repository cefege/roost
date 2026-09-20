// Visible terminal transport smoke assertions own per-session header evidence.
// Peer specs call these only after a route transition has reached a real browser.
// All visible matching headers are checked because one session can occupy several panes.
// Concrete routes require browser route proof; Waiting intentionally does not.

import type { Page } from "@playwright/test";
import { expect } from "./fixtures.ts";
import { readPeerRoute } from "./terminal-peer-helpers.ts";

type TerminalTransportIndicatorKind = "loopback" | "webrtc" | "sync" | null;

type VisibleIndicator = {
  readonly kind: string | null;
  readonly text: string;
};

const INDICATOR_TIMEOUT_MS = Number(process.env.ROOST_PEER_ROUTE_TIMEOUT_MS ?? 60_000);
const INDICATOR_INTERVALS_MS = [100, 250, 500] as const;
const COMPACT_CONTROL_TEST_IDS = [
  "mobile-deck-bar-menu",
  "tab-new",
  "mobile-tab-count",
] as const;

function labelForTransport(kind: TerminalTransportIndicatorKind): string {
  switch (kind) {
    case "loopback": return "Loopback";
    case "webrtc": return "WebRTC";
    case "sync": return "Coordinator";
    default: return "Waiting";
  }
}

function indicatorsMatch(
  indicators: readonly VisibleIndicator[],
  kind: TerminalTransportIndicatorKind,
): boolean {
  const label = labelForTransport(kind);
  const marker = kind ?? "unconfirmed";
  return indicators.length > 0
    && indicators.every((indicator) => indicator.text === label && indicator.kind === marker);
}

async function visibleIndicators(page: Page, sessionId: string): Promise<VisibleIndicator[]> {
  return page.locator(
    `[data-testid="terminal-transport-indicator"][data-session-id=${JSON.stringify(sessionId)}]:visible`,
  ).evaluateAll((elements) => elements.map((element) => ({
    kind: element.getAttribute("data-terminal-transport"),
    text: element.textContent?.trim() ?? "",
  })));
}

/** Proves every visible selected-pane carrier chip matches the elected browser route. */
export async function expectTerminalTransportIndicator(
  page: Page,
  sessionId: string,
  kind: "loopback" | "webrtc" | "sync" | null,
): Promise<void> {
  if (kind === null) {
    await expect.poll(
      () => visibleIndicators(page, sessionId).then((indicators) => indicatorsMatch(indicators, kind)),
      { timeout: INDICATOR_TIMEOUT_MS, intervals: [...INDICATOR_INTERVALS_MS] },
    ).toBe(true);
    return;
  }

  await expect.poll(async () => {
    const [route, indicators] = await Promise.all([
      readPeerRoute(page, sessionId),
      visibleIndicators(page, sessionId),
    ]);
    return route.activeKind === kind
      && route.proofKind === kind
      && route.baselineReady
      && indicatorsMatch(indicators, kind);
  }, { timeout: INDICATOR_TIMEOUT_MS, intervals: [...INDICATOR_INTERVALS_MS] }).toBe(true);
}

/** Proves the compact header retains its carrier label and its permanent controls. */
export async function expectCompactTerminalTransportHeader(
  page: Page,
  sessionId: string,
  kind: TerminalTransportIndicatorKind,
): Promise<void> {
  await expectTerminalTransportIndicator(page, sessionId, kind);
  const label = labelForTransport(kind);
  const marker = kind ?? "unconfirmed";
  await expect.poll(() => page.evaluate(({ controlTestIds, expectedLabel, expectedMarker, id }) => {
    const visible = (element: Element): element is HTMLElement => {
      const style = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return style.display !== "none"
        && style.visibility !== "hidden"
        && rect.width > 0
        && rect.height > 0;
    };
    const inViewport = (element: Element): boolean => {
      const rect = element.getBoundingClientRect();
      return rect.left >= -1
        && rect.top >= -1
        && rect.right <= window.innerWidth + 1
        && rect.bottom <= window.innerHeight + 1;
    };
    const compactBars = [...document.querySelectorAll('[data-testid="mobile-deck-bar"]')]
      .filter(visible);
    const indicators = compactBars.flatMap((bar) => [...bar.querySelectorAll(
      '[data-testid="terminal-transport-indicator"]',
    )]).filter(visible).filter((indicator) => indicator.getAttribute("data-session-id") === id);
    const controlsFit = controlTestIds.every((testId) => {
      const controls = [...document.querySelectorAll(`[data-testid="${testId}"]`)].filter(visible);
      return controls.length > 0 && controls.every(inViewport);
    });
    const indicatorsFit = indicators.length > 0 && indicators.every((indicator) => {
      const range = document.createRange();
      range.selectNodeContents(indicator);
      const textRects = [...range.getClientRects()];
      return indicator.getAttribute("data-terminal-transport") === expectedMarker
        && indicator.textContent?.trim() === expectedLabel
        && indicator.scrollWidth <= indicator.clientWidth
        && inViewport(indicator)
        && textRects.length > 0
        && textRects.every((rect) => (
          rect.left >= -1
          && rect.top >= -1
          && rect.right <= window.innerWidth + 1
          && rect.bottom <= window.innerHeight + 1
        ));
    });
    return compactBars.length > 0 && controlsFit && indicatorsFit;
  }, {
    controlTestIds: [...COMPACT_CONTROL_TEST_IDS],
    expectedLabel: label,
    expectedMarker: marker,
    id: sessionId,
  }), { timeout: INDICATOR_TIMEOUT_MS, intervals: [...INDICATOR_INTERVALS_MS] }).toBe(true);
}
