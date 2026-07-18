// Window size class — the single breakpoint vocabulary for the whole SPA,
// at Material 3's boundaries (Compact <600 / Medium 600-839 / Expanded >=840).
// Replaces the ad-hoc 767/768 matchMedia checks scattered across AppShell,
// MainPane, Terminal, TerminalContextMenu (which were also non-reactive — they
// read matchMedia().matches directly so layout didn't switch live on resize).
//
// This is a reactive Solid signal: components read isCompact() inside JSX and
// re-render when the window crosses a boundary. One module-level resize
// listener (rAF-debounced) feeds the signal for the app's lifetime.

import { createSignal } from "solid-js";

type WindowSizeClass = "compact" | "medium" | "expanded";

// M3 window size class boundaries (dp ≈ px at the SPA's 1x scale).
const COMPACT_MAX = 600; // < 600 → compact (phone)
const MEDIUM_MAX = 840;  // 600–839 → medium (tablet) ; >= 840 → expanded (desktop)

// Compact ("phone") is keyed on the SHORT side, not width, so a phone held
// in landscape (≈844×390 on an iPhone) still classifies compact and gets the
// full-screen sidebar — Author 2026-06-23 "even widescreen should show sidebar
// only". An iPad's short side is ≥744 so it stays medium/expanded. Medium vs
// expanded still splits on width (the live layout dimension).
function classify(width: number, height: number): WindowSizeClass {
  if (Math.min(width, height) < COMPACT_MAX) return "compact";
  if (width < MEDIUM_MAX) return "medium";
  return "expanded";
}

const initial: WindowSizeClass =
  typeof window !== "undefined" ? classify(window.innerWidth, window.innerHeight) : "expanded";

const [sizeClass, setSizeClass] = createSignal<WindowSizeClass>(initial);

if (typeof window !== "undefined") {
  let rafPending = 0;
  const recompute = () => {
    rafPending = 0;
    const next = classify(window.innerWidth, window.innerHeight);
    setSizeClass((prev) => (prev === next ? prev : next));
  };
  const onResize = () => {
    if (rafPending) return;
    rafPending = requestAnimationFrame(recompute);
  };
  window.addEventListener("resize", onResize);
  // Phones fire orientationchange without always firing resize in time.
  window.addEventListener("orientationchange", onResize);
}

/** Reactive accessor — read inside JSX to re-render on boundary crossings. */
const windowSizeClass = sizeClass;
export const isCompact = () => sizeClass() === "compact";

/** True on touch-primary devices (phones, tablets), where focusing the terminal
 *  pops the on-screen keyboard — so we suppress auto-focus on selection there
 *  and let an explicit tap open the keyboard. Static device capability. */
export function isTouchDevice(): boolean {
  try {
    // maxTouchPoints catches iPadOS Safari even when it mimics a desktop pointer.
    return window.matchMedia("(pointer: coarse)").matches || (navigator.maxTouchPoints ?? 0) > 0;
  } catch {
    return false;
  }
}
