// Owns modifier-gated hover and activation for terminal-rendered anchors.
// A separate scanner detects inferred links, while the shared DOM applier
// validates and authors both inferred and producer-painted targets.
// CellTerminal attaches one instance and coordinates its repaint hold.

import {
  classifyTerminalLinkTarget,
  computeRowLinks,
  isWorkerFileHref,
} from "./terminal-links.detect.ts";
import type {
  PaintedLink,
  ResolveFile,
  RowLinkInput,
  RowLinkSegment,
  TerminalLinkTarget,
} from "./terminal-links.detect.ts";
import {
  TERMINAL_LINK_CLASS as LINK_CLASS,
  TERMINAL_LINK_TARGET_ATTR,
} from "../lib/cellRow.ts";
import { terminalLinkModifierKey } from "../lib/browserPlatform.ts";
import {
  applyTerminalAnchorTarget,
  resolveTerminalAnchorTarget,
} from "./terminal-links.dom.ts";
import { attachTerminalLinkScanner } from "./terminal-links.scan.ts";

export {
  classifyTerminalLinkTarget,
  computeRowLinks,
  isWorkerFileHref,
};
export type {
  PaintedLink,
  ResolveFile,
  RowLinkInput,
  RowLinkSegment,
  TerminalLinkTarget,
};

// Kept here so this legacy raw font value remains attributed to its existing
// design-ratchet baseline while the interaction attachment loads it lazily.
const CSS_INJECTED = Symbol.for("roost.wterm-link.css");
function injectTerminalLinkCssOnce(): void {
  if ((globalThis as Record<symbol, unknown>)[CSS_INJECTED]) return;
  (globalThis as Record<symbol, unknown>)[CSS_INJECTED] = true;
  const style = document.createElement("style");
  style.setAttribute("data-roost", "wterm-link");
  style.textContent = `
.${LINK_CLASS} {
  color: inherit;
  text-decoration: none;
  pointer-events: auto;
  cursor: text;
}
.wterm[data-link-armed="1"] .${LINK_CLASS} {
  text-decoration: underline;
  text-underline-offset: 2px;
  cursor: pointer;
}
/* File links pick up the accent so they read as "opens in Roost", not the web. */
.wterm[data-link-armed="1"] .${LINK_CLASS}[data-kind="file"] {
  text-decoration-color: var(--md-primary, currentColor);
}
.wterm-link-hint {
  position: fixed;
  z-index: 2147483000;
  display: none;
  max-width: 60vw;
  padding: 3px 8px;
  border-radius: var(--md-shape-sm, 6px);
  background: var(--surface-2);
  color: var(--text-hi);
  border: 1px solid var(--border-subtle);
  box-shadow: var(--md-elev-3);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px;
  line-height: 1.4;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  pointer-events: none;
}
`;
  document.head.appendChild(style);
}

export interface TerminalLinkActivationGesture {
  button: number;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

/** The event is authoritative; window modifier state is presentation only. */
export function isTerminalLinkActivationGesture(
  event: TerminalLinkActivationGesture,
): boolean {
  if (event.button !== 0 || event.shiftKey || event.altKey) return false;
  return terminalLinkModifierKey() === "Meta"
    ? event.metaKey && !event.ctrlKey
    : event.ctrlKey && !event.metaKey;
}

export interface TerminalLinkAttachment {
  releaseInteraction(): void;
  openLink(anchor: HTMLAnchorElement): boolean;
  describeLink(anchor: HTMLAnchorElement): string | null;
  dispose(): void;
}

export interface TerminalLinkOpts {
  /** Resolve output paths into authenticated `/file/…` routes. */
  resolveFile?: ResolveFile;
  onOpenFile?: (href: string) => void;
  /** Getter so scans see a Git remote that resolves after pane mount. */
  githubOwnerRepo?: () => string | undefined;
  /** Holds renderer paint only while the modifier and pointer are both active. */
  onArmedHoverChange?: (active: boolean) => void;
}

export function attachTerminalLinks(
  container: HTMLElement,
  opts: TerminalLinkOpts = {},
): TerminalLinkAttachment {
  injectTerminalLinkCssOnce();
  const scanner = attachTerminalLinkScanner(container, opts);
  const modKey = terminalLinkModifierKey();
  let armed = false;
  let pointerInside = false;
  let holding = false;

  const recomputeHold = (): void => {
    const next = armed && pointerInside;
    if (next === holding) return;
    holding = next;
    opts.onArmedHoverChange?.(next);
    // Repaint can have replaced every inferred anchor since the last hover.
    if (armed) scanner.requestFullScan();
  };
  const setArmed = (next: boolean): void => {
    if (next === armed) return;
    armed = next;
    if (next) container.setAttribute("data-link-armed", "1");
    else container.removeAttribute("data-link-armed");
    recomputeHold();
  };
  const modifierHeld = (event: Pick<MouseEvent, "ctrlKey" | "metaKey">): boolean =>
    modKey === "Meta" ? event.metaKey : event.ctrlKey;
  const onKeyDown = (event: KeyboardEvent): void => {
    if (event.key === modKey) setArmed(true);
  };
  const onKeyUp = (event: KeyboardEvent): void => {
    if (event.key === modKey) setArmed(false);
  };

  let hintElement: HTMLDivElement | null = null;
  const hideHint = (): void => {
    if (hintElement) hintElement.style.display = "none";
  };
  const releaseInteraction = (): void => {
    container.removeAttribute("data-link-armed");
    armed = false;
    pointerInside = false;
    recomputeHold();
    hideHint();
  };
  const onPointerEnter = (event: MouseEvent): void => {
    pointerInside = true;
    if (modifierHeld(event)) setArmed(true);
    recomputeHold();
  };
  const onPointerLeave = (): void => {
    pointerInside = false;
    recomputeHold();
  };
  const onPointerModifiers = (event: MouseEvent): void => {
    setArmed(modifierHeld(event));
  };
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("keyup", onKeyUp);
  window.addEventListener("blur", releaseInteraction);
  container.addEventListener("mouseenter", onPointerEnter);
  container.addEventListener("mouseleave", onPointerLeave);
  container.addEventListener("mousemove", onPointerModifiers);
  container.addEventListener("mousedown", onPointerModifiers);

  const showHint = (anchor: HTMLElement): void => {
    const text = anchor.dataset.hint;
    if (!text) return;
    if (!hintElement) {
      hintElement = document.createElement("div");
      hintElement.className = "wterm-link-hint";
      document.body.appendChild(hintElement);
    }
    hintElement.textContent =
      `${modKey === "Meta" ? "⌘-click" : "Ctrl-click"} to open · ${text.replace(/^Open /, "")}`;
    const rect = anchor.getBoundingClientRect();
    hintElement.style.left = `${Math.round(rect.left)}px`;
    hintElement.style.top = `${Math.round(rect.bottom + 4)}px`;
    hintElement.style.display = "block";
  };
  const anchorFrom = (target: EventTarget | null): HTMLAnchorElement | null =>
    (target as Element | null)?.closest?.(`a.${LINK_CLASS}`) as HTMLAnchorElement | null;
  const onOver = (event: MouseEvent): void => {
    if (modifierHeld(event)) setArmed(true);
    if (!armed) return;
    const anchor = anchorFrom(event.target);
    if (anchor) showHint(anchor);
    else hideHint();
  };
  const onOut = (event: MouseEvent): void => {
    if (anchorFrom(event.target)) hideHint();
  };
  const describeLink = (anchor: HTMLAnchorElement): string | null =>
    resolveTerminalAnchorTarget(anchor, opts.resolveFile)?.display ?? null;
  const openLink = (anchor: HTMLAnchorElement): boolean => {
    const target = resolveTerminalAnchorTarget(anchor, opts.resolveFile);
    if (!target) return false;
    const rawTarget = anchor.getAttribute(TERMINAL_LINK_TARGET_ATTR) ?? target.display;
    applyTerminalAnchorTarget(anchor, rawTarget, target);
    if (target.kind === "file") {
      if (!target.href || !opts.onOpenFile) return false;
      opts.onOpenFile(target.href);
    } else {
      const nativeAnchor = document.createElement("a");
      applyTerminalAnchorTarget(nativeAnchor, rawTarget, target);
      nativeAnchor.setAttribute("style", "display:none");
      document.body.appendChild(nativeAnchor);
      nativeAnchor.click();
      nativeAnchor.remove();
    }
    hideHint();
    return true;
  };
  const onClick = (event: MouseEvent): void => {
    const anchor = anchorFrom(event.target);
    if (!anchor) return;
    const target = resolveTerminalAnchorTarget(anchor, opts.resolveFile);
    if (!target || !isTerminalLinkActivationGesture(event)) {
      event.preventDefault();
      return;
    }
    const rawTarget = anchor.getAttribute(TERMINAL_LINK_TARGET_ATTR) ?? target.display;
    applyTerminalAnchorTarget(anchor, rawTarget, target);
    if (target.kind === "file") {
      event.preventDefault();
      if (target.href && opts.onOpenFile) opts.onOpenFile(target.href);
    }
    hideHint();
  };
  container.addEventListener("mouseover", onOver);
  container.addEventListener("mouseout", onOut);
  container.addEventListener("click", onClick);

  const dispose = (): void => {
    scanner.dispose();
    window.removeEventListener("keydown", onKeyDown);
    window.removeEventListener("keyup", onKeyUp);
    window.removeEventListener("blur", releaseInteraction);
    container.removeEventListener("mouseover", onOver);
    container.removeEventListener("mouseout", onOut);
    container.removeEventListener("mouseenter", onPointerEnter);
    container.removeEventListener("mouseleave", onPointerLeave);
    container.removeEventListener("mousemove", onPointerModifiers);
    container.removeEventListener("mousedown", onPointerModifiers);
    container.removeEventListener("click", onClick);
    releaseInteraction();
    hintElement?.remove();
    hintElement = null;
  };
  return { releaseInteraction, openLink, describeLink, dispose };
}
