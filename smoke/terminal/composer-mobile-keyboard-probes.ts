import type { Locator, Page } from "@playwright/test";

// Page probes private to composer-mobile-keyboard.spec.ts. They were inline
// closures in that test and depend on nothing but the page/session or the pane
// Locator, so they live here to keep the spec under the repo's file-size cap.
// Bodies are unchanged; the spec still calls them through same-named closures.

export function mobilePaneFocused(page: Page, sessionId: string) {
  return page.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { paneFocused(sessionId: string): { focused: boolean } };
    }).__smoke;
    return smoke.paneFocused(id).focused;
  }, sessionId);
}

export function readMobileKeyboardGeometry(page: Page, sessionId: string) {
  return page.evaluate((id) => {
    const terminal = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const composerDock = document.querySelector('[data-testid="mobile-chat-input"]');
    const chatBox = document.querySelector('[data-testid="chat-box"]');
    const keyToggle = document.querySelector('[data-testid="terminal-nav-toggle"]');
    const keyPanel = document.querySelector('[data-testid="terminal-nav-buttons"]');
    if (
      !(terminal instanceof HTMLElement)
      || !(composerDock instanceof HTMLElement)
      || !(chatBox instanceof HTMLElement)
      || !(keyToggle instanceof HTMLElement)
      || (keyPanel !== null && !(keyPanel instanceof HTMLElement))
    ) return null;

    const readRect = (element: HTMLElement) => {
      const rect = element.getBoundingClientRect();
      return {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
        width: rect.width,
        height: rect.height,
      };
    };
    const terminalRect = readRect(terminal);
    const dockRect = readRect(composerDock);
    const chatRect = readRect(chatBox);
    const toggleRect = readRect(keyToggle);
    const panelRect = keyPanel ? readRect(keyPanel) : null;
    const viewport = window.visualViewport;
    const viewportLeft = viewport?.offsetLeft ?? 0;
    const viewportTop = viewport?.offsetTop ?? 0;
    const viewportRight = viewportLeft + (viewport?.width ?? window.innerWidth);
    const viewportBottom = viewportTop + (viewport?.height ?? window.innerHeight);
    const panelStyle = keyPanel ? getComputedStyle(keyPanel) : null;
    return {
      terminal: terminalRect,
      dock: dockRect,
      chat: chatRect,
      toggle: toggleRect,
      panel: panelRect,
      detached: !composerDock.contains(keyToggle)
        && !chatBox.contains(keyToggle)
        && (!keyPanel || (!composerDock.contains(keyPanel) && !chatBox.contains(keyPanel))),
      togglePosition: getComputedStyle(keyToggle).position,
      panelPosition: panelStyle?.position ?? null,
      panelAboveToggle: panelRect ? panelRect.bottom <= toggleRect.top + 1 : null,
      surfacesInside: [toggleRect, chatRect, ...(panelRect ? [panelRect] : [])].every((rect) =>
        rect.left >= viewportLeft - 1
        && rect.top >= viewportTop - 1
        && rect.right <= viewportRight + 1
        && rect.bottom <= viewportBottom + 1),
      panelOverflowY: panelStyle?.overflowY ?? null,
      panelScrollHeight: keyPanel?.scrollHeight ?? null,
      panelClientHeight: keyPanel?.clientHeight ?? null,
    };
  }, sessionId);
}

export function measureMobileWheelTarget(slot: Locator) {
  return slot.locator(".cell-grid").evaluate((element) => {
    if (!(element instanceof HTMLElement)) throw new Error("mobile terminal grid disappeared");
    const viewport = element.querySelector(".cell-viewport");
    const row = viewport?.querySelector(".cell-row");
    if (!(viewport instanceof HTMLElement) || !(row instanceof HTMLElement)) {
      throw new Error("mobile terminal cell geometry disappeared");
    }
    const gridRect = element.getBoundingClientRect();
    const cols = Number.parseInt(element.style.getPropertyValue("--cell-cols"), 10);
    const cellWidth = viewport.getBoundingClientRect().width / cols;
    const cellHeight = row.getBoundingClientRect().height;
    if (!Number.isFinite(cellWidth) || cellWidth <= 0 || cellHeight <= 0) {
      throw new Error("mobile terminal cell geometry is invalid");
    }
    // The detached key sheet covers the grid center. Aim the trusted wheel at
    // the unobscured top-left cell instead of accidentally scrolling the sheet.
    const x = gridRect.left + cellWidth / 2;
    const y = gridRect.top + cellHeight / 2;
    const hit = document.elementFromPoint(x, y);
    if (!hit || !element.contains(hit)) {
      throw new Error("mobile terminal forwarded-wheel target is occluded");
    }
    return {
      x,
      y,
      col: Math.max(1, 1 + Math.floor((x - gridRect.left) / cellWidth)),
      row: Math.max(1, 1 + Math.floor((y - gridRect.top) / cellHeight)),
    };
  });
}
