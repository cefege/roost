import type { Comparison } from "../scenario";
import { contentComparison } from "./gallery-support";

export const overlayDialog = "body";
export const fullscreenOverlay = { overlay: true, overlayOptions: { anchor: "top-left", width: "100%", maxHeight: "100%", margin: 0, fullscreen: true } } as const;

export const componentId = (file: string, name: string) => `component:packages/tui/src/${file}.ts#${name}`;
export const agentComponentId = (file: string, name: string) => `component:packages/coding-agent/src/${file}.ts#${name}`;

export function shown(name: string, token: string, selector: string = overlayDialog): Comparison {
  const escaped = token.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return contentComparison(name, `(${escaped})`, selector, token);
}

export function shownAs(name: string, terminalPattern: string, browserPattern: string, expected: string, selector: string = overlayDialog): Comparison {
  return {
    name,
    terminal: { type: "screen", pattern: terminalPattern, group: 1 },
    browser: { type: "dom", selector, read: "text", pattern: browserPattern, group: 1 },
    expected,
    normalize: "whitespace",
  };
}
