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

function syncKeyboardInset(): void {
  const vv = window.visualViewport;
  if (!vv) {
    document.documentElement.style.setProperty("--kb-offset", "0px");
    return;
  }
  // Layout viewport stays full (resizes-visual); the keyboard covers the
  // bottom = innerHeight - (visible height + how far the visual viewport is
  // offset down). Clamp to >=0 and ignore sub-pixel jitter / tiny insets.
  const covered = window.innerHeight - vv.height - vv.offsetTop;
  const inset = covered > 80 ? Math.round(covered) : 0;
  document.documentElement.style.setProperty("--kb-offset", `${inset}px`);
}

if (typeof window !== "undefined") {
  syncKeyboardInset();
  let rafPending = 0;
  const onChange = () => {
    if (rafPending) return;
    rafPending = requestAnimationFrame(() => {
      rafPending = 0;
      syncKeyboardInset();
    });
  };
  window.visualViewport?.addEventListener("resize", onChange);
  window.visualViewport?.addEventListener("scroll", onChange);
  window.addEventListener("orientationchange", onChange);
}
