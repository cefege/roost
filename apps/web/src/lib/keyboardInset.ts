// Mobile soft-keyboard inset → --kb-offset (px).
//
// We do NOT resize/reflow the terminal when the keyboard opens (that resizes
// the PTY + re-wraps every pane — disruptive, especially with multiple
// terminals). Instead the keyboard overlays (viewport meta
// interactive-widget=resizes-visual) and we publish how much of the layout
// the keyboard covers into --kb-offset; AppShell translates the content up by
// that amount so the input rides just above the keyboard, the top scrolls
// off, and the terminal keeps its exact size + grid.
//
// Imported for side effect by main.tsx. Covers iOS Safari (VisualViewport)
// + Chrome (resizes-visual keeps layout full, visualViewport reports the
// shrink either way).

const MIN_KEYBOARD_COVERAGE_PX = 80;
const SETTLE_DELAY_1_MS = 80;
const SETTLE_DELAY_2_MS = 240;
const SETTLE_DELAY_3_MS = 500;

function isKeyboardInput(target: EventTarget | null): target is HTMLElement {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;

  if (target.tagName === "TEXTAREA") {
    const textarea = target as HTMLTextAreaElement;
    return !textarea.disabled && !textarea.readOnly;
  }
  if (target.tagName !== "INPUT") return false;

  const input = target as HTMLInputElement;
  if (input.disabled || input.readOnly) return false;
  switch (input.type) {
    case "button":
    case "checkbox":
    case "color":
    case "file":
    case "hidden":
    case "image":
    case "radio":
    case "range":
    case "reset":
    case "submit":
      return false;
    default:
      return true;
  }
}

function hasFocusedKeyboardInput(): boolean {
  let active: Element | null = document.activeElement;
  while (active) {
    if (isKeyboardInput(active)) return true;
    active = active.shadowRoot?.activeElement ?? null;
  }
  return false;
}

function focusEventHasKeyboardInput(event: FocusEvent): boolean {
  if (isKeyboardInput(event.target)) return true;
  for (const target of event.composedPath()) {
    if (isKeyboardInput(target ?? null)) return true;
  }
  return hasFocusedKeyboardInput();
}

if (typeof window !== "undefined") {
  const rootStyle = document.documentElement.style;
  let keyboardFocused = hasFocusedKeyboardInput();
  let sampleRaf = 0;
  let settleRaf = 0;
  let delayedSample1 = 0;
  let delayedSample2 = 0;
  let delayedSample3 = 0;
  let settlingAfterReset = false;

  function clearKeyboardInset(): void {
    rootStyle.setProperty("--kb-offset", "0px");
  }

  function syncKeyboardInset(): void {
    const vv = window.visualViewport;
    if (!keyboardFocused || !vv) {
      clearKeyboardInset();
      return;
    }

    // Layout viewport stays full (resizes-visual); the keyboard covers the
    // bottom = innerHeight - (visible height + how far the visual viewport is
    // offset down). Clamp to >=0 and ignore sub-pixel jitter / tiny insets.
    const covered = window.innerHeight - vv.height - vv.offsetTop;
    const inset = covered > MIN_KEYBOARD_COVERAGE_PX ? Math.round(covered) : 0;
    rootStyle.setProperty("--kb-offset", `${inset}px`);
  }

  function runSample(): void {
    sampleRaf = 0;
    syncKeyboardInset();
  }

  function scheduleSample(): void {
    if (!sampleRaf) sampleRaf = requestAnimationFrame(runSample);
  }

  function cancelScheduledWork(): void {
    cancelAnimationFrame(sampleRaf);
    cancelAnimationFrame(settleRaf);
    clearTimeout(delayedSample1);
    clearTimeout(delayedSample2);
    clearTimeout(delayedSample3);
    sampleRaf = 0;
    settleRaf = 0;
    delayedSample1 = 0;
    delayedSample2 = 0;
    delayedSample3 = 0;
    settlingAfterReset = false;
  }

  function sampleAfterSecondFrame(): void {
    settleRaf = 0;
    settlingAfterReset = false;
    syncKeyboardInset();
  }

  function waitForSecondFrame(): void {
    settleRaf = requestAnimationFrame(sampleAfterSecondFrame);
  }

  function runDelayedSample1(): void {
    delayedSample1 = 0;
    scheduleSample();
  }

  function runDelayedSample2(): void {
    delayedSample2 = 0;
    scheduleSample();
  }

  function runDelayedSample3(): void {
    delayedSample3 = 0;
    scheduleSample();
  }

  function sampleAfterLayoutSettles(blockEarlyViewportEvents: boolean): void {
    settlingAfterReset = blockEarlyViewportEvents;
    settleRaf = requestAnimationFrame(waitForSecondFrame);
    delayedSample1 = window.setTimeout(runDelayedSample1, SETTLE_DELAY_1_MS);
    delayedSample2 = window.setTimeout(runDelayedSample2, SETTLE_DELAY_2_MS);
    delayedSample3 = window.setTimeout(runDelayedSample3, SETTLE_DELAY_3_MS);
  }

  function resetAndResample(): void {
    cancelScheduledWork();
    clearKeyboardInset();
    keyboardFocused = hasFocusedKeyboardInput();
    sampleAfterLayoutSettles(true);
  }

  function onViewportChange(): void {
    if (!keyboardFocused) {
      clearKeyboardInset();
      return;
    }
    // orientationchange fires before VisualViewport has adopted the new
    // dimensions on Safari. Its reset path owns the first settled sample.
    if (!settlingAfterReset) scheduleSample();
  }

  function onFocus(event: FocusEvent): void {
    if (!focusEventHasKeyboardInput(event)) return;
    if (keyboardFocused) {
      onViewportChange();
      return;
    }
    keyboardFocused = true;
    cancelScheduledWork();
    clearKeyboardInset();
    sampleAfterLayoutSettles(false);
  }

  function onBlur(): void {
    keyboardFocused = false;
    cancelScheduledWork();
    clearKeyboardInset();
  }

  syncKeyboardInset();
  window.visualViewport?.addEventListener("resize", onViewportChange);
  window.visualViewport?.addEventListener("scroll", onViewportChange);
  window.addEventListener("resize", onViewportChange);
  window.addEventListener("orientationchange", resetAndResample);
  window.addEventListener("pageshow", resetAndResample);
  window.addEventListener("focus", onFocus, true);
  window.addEventListener("blur", onBlur, true);
}
