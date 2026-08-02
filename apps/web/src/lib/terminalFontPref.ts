// Terminal zoom. Owns the `--term-font-size` CSS variable that `.wterm` and
// `.cell-grid` read (styles/sidebar.css), persisted per browser so a pane opens
// at the size the user last chose.
//
// Changing it changes the measured cell box, which changes cols/rows for a fixed
// pane — so every CellTerminal re-measures and re-claims off this signal. That
// really is a PTY resize round trip; it is unavoidable and correct.

import { createSignal } from "solid-js";

const KEY = "roost.termFontSize";
const DEFAULT_PX = 14;
// Below 9px the cell box stops being legible; above 28px a normal pane holds so
// few columns that most TUIs letterbox into uselessness.
export const TERM_FONT_MIN_PX = 9;
export const TERM_FONT_MAX_PX = 28;

function readStored(): number {
  if (typeof localStorage === "undefined") return DEFAULT_PX;
  const raw = Number(localStorage.getItem(KEY));
  return Number.isFinite(raw) && raw > 0 ? clamp(raw) : DEFAULT_PX;
}

function clamp(px: number): number {
  return Math.min(TERM_FONT_MAX_PX, Math.max(TERM_FONT_MIN_PX, Math.round(px)));
}

const [termFontSize, _setTermFontSize] = createSignal(readStored());
export { termFontSize };

/** Write the new size through to the document so CSS, the cell measurement and
 *  the persisted preference can never disagree. */
export function setTermFontSize(px: number): void {
  const next = clamp(px);
  if (next === termFontSize()) return;
  _setTermFontSize(next);
  applyTermFontSize();
  try { localStorage.setItem(KEY, String(next)); } catch { /* private mode / quota */ }
}

export function stepTermFontSize(delta: number): void {
  setTermFontSize(termFontSize() + delta);
}

export function resetTermFontSize(): void {
  setTermFontSize(DEFAULT_PX);
}

/** Push the current value onto the document element. Called once at boot (before
 *  the first pane measures) and on every change. */
export function applyTermFontSize(): void {
  if (typeof document === "undefined") return;
  document.documentElement.style.setProperty("--term-font-size", `${termFontSize()}px`);
}
