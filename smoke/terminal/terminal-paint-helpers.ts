import type { Page } from "@playwright/test";
import type {
  PaintedCursorProof,
  PaintedMarkerProof,
} from "../../apps/web/src/lib/smokeHarness.ts";
import type {
  RecoverySmokeApi,
  PaintAttempt,
  ImmediateTerminalPaintSample,
} from "./terminal-smoke-api.ts";

export async function holdNativeTerminalSelection(page: Page, sessionId: string): Promise<boolean> {
  return page.evaluate((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const terminal = slot?.querySelector(".wterm");
    if (!(terminal instanceof HTMLElement)) return false;
    const clip = terminal.getBoundingClientRect();
    const rows = Array.from(slot?.querySelectorAll(".cell-row") ?? []);
    const row = rows.find((candidate) => {
      if (!(candidate instanceof HTMLElement) || (candidate.textContent ?? "").length === 0) return false;
      const rect = candidate.getBoundingClientRect();
      return rect.height > 0 && rect.width > 0 && rect.bottom > clip.top && rect.top < clip.bottom;
    });
    if (!row) return false;
    const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
    let text = walker.nextNode();
    while (text && !text.textContent) text = walker.nextNode();
    if (!text?.textContent) return false;
    const range = document.createRange();
    range.setStart(text, 0);
    range.setEnd(text, 1);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    document.dispatchEvent(new Event("selectionchange"));
    return !!selection && !selection.isCollapsed;
  }, sessionId);
}

export async function attemptPaintedMarker(
  page: Page,
  sessionId: string,
  marker: string,
  timeoutMs = 750,
): Promise<PaintAttempt<PaintedMarkerProof>> {
  return page.evaluate(async ({ id, expected, timeout }) => {
    try {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      const proof = await smokeWindow.__smoke.waitForPaintedMarker(id, expected, timeout);
      return { proof, error: null };
    } catch (error) {
      return { proof: null, error: error instanceof Error ? error.message : String(error) };
    }
  }, { id: sessionId, expected: marker, timeout: timeoutMs });
}

export async function attemptPaintedCursor(
  page: Page,
  sessionId: string,
  expected: { row?: number; column?: number },
  timeoutMs = 750,
): Promise<PaintAttempt<PaintedCursorProof>> {
  return page.evaluate(async ({ id, target, timeout }) => {
    try {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      const proof = await smokeWindow.__smoke.waitForPaintedCursor(id, target, timeout);
      return { proof, error: null };
    } catch (error) {
      return { proof: null, error: error instanceof Error ? error.message : String(error) };
    }
  }, { id: sessionId, target: expected, timeout: timeoutMs });
}

export async function armImmediateTerminalPaintSample(
  page: Page,
  sessionId: string,
  eventType: "click" | "input" | "keydown" | "paste" | "wheel",
  expected: { marker?: string; cursorRow?: number; targetTestId?: string },
): Promise<void> {
  await page.evaluate(({ id, observedEvent, marker, cursorRow, targetTestId }) => {
    interface ImmediatePaintRuntime {
      sample: ImmediateTerminalPaintSample | null;
      cleanup: (() => void) | null;
    }
    const runtimeWindow = window as unknown as Window & {
      __immediateTerminalPaint?: ImmediatePaintRuntime;
    };
    runtimeWindow.__immediateTerminalPaint?.cleanup?.();
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const terminal = slot?.querySelector(".cell-grid");
    const viewport = terminal?.querySelector(".cell-viewport");
    if (
      !(slot instanceof HTMLElement)
      || !(terminal instanceof HTMLElement)
      || !(viewport instanceof HTMLElement)
    ) throw new Error("terminal paint sample could not resolve the live cell surface");

    const originalCursorRow = cursorRow === undefined
      ? null
      : viewport.querySelectorAll(".cell-row").item(cursorRow);
    const rect = (value: DOMRect) => ({
      left: value.left,
      top: value.top,
      right: value.right,
      bottom: value.bottom,
    });
    const visiblyInside = (element: HTMLElement): boolean => {
      const value = element.getBoundingClientRect();
      const clip = terminal.getBoundingClientRect();
      const viewportLeft = window.visualViewport?.offsetLeft ?? 0;
      const viewportTop = window.visualViewport?.offsetTop ?? 0;
      const viewportRight = viewportLeft + (window.visualViewport?.width ?? window.innerWidth);
      const viewportBottom = viewportTop + (window.visualViewport?.height ?? window.innerHeight);
      return value.width > 0
        && value.height > 0
        && value.right > Math.max(clip.left, viewportLeft)
        && value.left < Math.min(clip.right, viewportRight)
        && value.bottom > Math.max(clip.top, viewportTop)
        && value.top < Math.min(clip.bottom, viewportBottom);
    };
    const runtime: ImmediatePaintRuntime = { sample: null, cleanup: null };
    const onEvent = (event: Event) => {
      if (!(event.target instanceof Node)) return;
      const outsideTarget = event.target instanceof Element && targetTestId !== undefined
        ? event.target.closest(`[data-testid="${CSS.escape(targetTestId)}"]`)
        : null;
      if (!slot.contains(event.target) && !outsideTarget) return;
      document.removeEventListener(observedEvent, onEvent);
      runtime.cleanup = null;
      const cursor = viewport.querySelector(".cell-cursor");
      const markerRow = marker === undefined
        ? null
        : Array.from(terminal.querySelectorAll(".cell-row")).find((row) =>
          (row.textContent ?? "").includes(marker));
      runtime.sample = {
        eventType: event.type,
        trusted: event.isTrusted,
        selectionCollapsed: window.getSelection()?.isCollapsed ?? true,
        cursorRow: cursor instanceof HTMLElement && Number.isSafeInteger(Number(cursor.dataset.row))
          ? Number(cursor.dataset.row)
          : null,
        cursorColumn: cursor instanceof HTMLElement && Number.isSafeInteger(Number(cursor.dataset.column))
          ? Number(cursor.dataset.column)
          : null,
        cursorRect: cursor instanceof HTMLElement && visiblyInside(cursor)
          ? rect(cursor.getBoundingClientRect())
          : null,
        markerRowRect: markerRow instanceof HTMLElement && visiblyInside(markerRow)
          ? rect(markerRow.getBoundingClientRect())
          : null,
        composerHeight: (() => {
          const composer = slot.querySelector('[data-testid="mobile-chat-input"]');
          return composer instanceof HTMLElement ? composer.getBoundingClientRect().height : null;
        })(),
        cursorRowIdentity: cursorRow === undefined
          ? null
          : originalCursorRow !== null
            && viewport.querySelectorAll(".cell-row").item(cursorRow) === originalCursorRow,
      };
    };
    runtime.cleanup = () => document.removeEventListener(observedEvent, onEvent);
    runtimeWindow.__immediateTerminalPaint = runtime;
    document.addEventListener(observedEvent, onEvent);
  }, {
    id: sessionId,
    observedEvent: eventType,
    marker: expected.marker,
    cursorRow: expected.cursorRow,
    targetTestId: expected.targetTestId,
  });
}

/** Chromium delivers a dispatched input event to JS asynchronously: the
 *  `mouse.wheel`/`keyboard` call resolves on the CDP ack, not on the listener
 *  having run. Reading the armed sample straight after the gesture therefore
 *  races it and observes null under load. Wait for the sample the arm exposes,
 *  then read it — still null after the budget means the gesture genuinely never
 *  reached the pane. */
export async function readImmediateTerminalPaintSample(page: Page): Promise<ImmediateTerminalPaintSample | null> {
  await page.waitForFunction(() => {
    const runtimeWindow = window as unknown as Window & {
      __immediateTerminalPaint?: { sample: ImmediateTerminalPaintSample | null };
    };
    return runtimeWindow.__immediateTerminalPaint?.sample != null;
  }, undefined, { timeout: 5_000 }).catch(() => undefined);
  return page.evaluate(() => {
    const runtimeWindow = window as unknown as Window & {
      __immediateTerminalPaint?: { sample: ImmediateTerminalPaintSample | null };
    };
    return runtimeWindow.__immediateTerminalPaint?.sample ?? null;
  });
}
