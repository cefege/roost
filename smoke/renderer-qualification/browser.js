// Renderer qualification browser surface — isolated published DOM Renderer mount.
// server.ts bundles this file and serves no production web assets.
// The browser API exposes deterministic fixture actions to qualify.ts only.
// Loading and expiry notices intentionally live outside terminal rows.

import "@wterm/dom/css";
import { mountQualificationProbe } from "./probe-source.js";

const surface = document.querySelector("#qualification-surface");
const grid = document.querySelector("#qualification-grid");
const loading = document.querySelector("#qualification-notice");
if (!(surface instanceof HTMLElement) || !(grid instanceof HTMLElement) || !(loading instanceof HTMLElement)) {
  throw new Error("QualificationSurfaceMissing");
}

const probe = mountQualificationProbe({ surface, grid, loading });

function selectMarker(marker) {
  const textNodes = document.createTreeWalker(grid, NodeFilter.SHOW_TEXT);
  let textNode = textNodes.nextNode();
  while (textNode !== null) {
    const position = textNode.textContent?.indexOf(marker) ?? -1;
    if (position >= 0) {
      const range = document.createRange();
      range.setStart(textNode, position);
      range.setEnd(textNode, position + marker.length);
      const selection = document.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
      return selection?.toString() ?? "";
    }
    textNode = textNodes.nextNode();
  }
  throw new Error(`MarkerNotPainted:${marker}`);
}

window.__rendererQualification = {
  ready: probe.coldBottom(),
  snapshot: () => probe.snapshot(),
  refresh: () => probe.refresh(),
  scrollToAbsolute: (absoluteRow) => probe.scrollToAbsolute(absoluteRow),
  selectMarker,
  collapseSelection: () => probe.collapseSelection(),
  queueFrame: (cols) => probe.queueFrame(cols),
  evictThrough: (floor) => probe.evictThrough(floor),
  anchor: (absoluteRow) => probe.anchor(absoluteRow),
  stalePageThenReplace: () => probe.stalePageThenReplace(),
  destroy: () => probe.destroy(),
};
