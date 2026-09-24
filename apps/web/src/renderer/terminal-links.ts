// Owns physical-modifier hover and physical or compact-sheet activation for terminal-rendered anchors.
// A separate scanner detects inferred links, while the shared DOM applier
// validates and authors both inferred and producer-painted targets.
// CellTerminal attaches one instance and coordinates its repaint hold.

import type { Accessor } from "solid-js";
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
} from "./cellRow.ts";
import { terminalLinkModifierKey } from "../browser/browserPlatform.ts";
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
const NO_LINK_ACTIVATION_ARMED: Accessor<boolean> = () => false;
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

/** Physical modifiers come from the event; compact arming is pane-local state. */
export function isTerminalLinkActivationGesture(
  event: TerminalLinkActivationGesture,
  linkActivationArmed: Accessor<boolean>,
): boolean {
  if (event.button !== 0 || event.shiftKey || event.altKey) return false;
  if (linkActivationArmed()) return true;
  return terminalLinkModifierKey() === "Meta"
    ? event.metaKey && !event.ctrlKey
    : event.ctrlKey && !event.metaKey;
}

export interface TerminalLinkAttachment {
  setActive(active: boolean): void;
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
  /** Compact keyboard-sheet state, separate from physical modifier hover. */
  linkActivationArmed?: Accessor<boolean>;
  /** Foreground state at construction; hidden panes install no link work. */
  initialActive?: boolean;
  /** Holds renderer paint only while the modifier and pointer are both active. */
  onArmedHoverChange?: (active: boolean) => void;
}

export function attachTerminalLinks(
  container: HTMLElement,
  opts: TerminalLinkOpts = {},
): TerminalLinkAttachment {
  injectTerminalLinkCssOnce();
  const initialActive = opts.initialActive ?? true;
  const linkActivationArmed =
    opts.linkActivationArmed ?? NO_LINK_ACTIVATION_ARMED;
  const scanner = attachTerminalLinkScanner(container, opts, initialActive);
  const modKey = terminalLinkModifierKey();
  let active = initialActive;
  let disposed = false;
  let armed = false;
  let pointerInside = false;
  let holding = false;

  const recomputeHold = (): void => {
    const next = armed && pointerInside;
    if (next === holding) return;
    holding = next;
    opts.onArmedHoverChange?.(next);
    // Repaint can have replaced inferred anchors in the current terminal tail.
    if (active && armed) scanner.requestCurrentScan();
  };
  const setArmed = (next: boolean): void => {
    if (next === armed) return;
    armed = next;
    if (next) container.setAttribute("data-link-armed", "1");
    else container.removeAttribute("data-link-armed");
    recomputeHold();
  };
  // A LEVEL, so it must be total: an event that carries no modifier field at
  // all states "not held", never an absent third value that would propagate
  // into the hold as neither armed nor disarmed.
  const modifierHeld = (event: Pick<MouseEvent, "ctrlKey" | "metaKey">): boolean =>
    modKey === "Meta" ? event.metaKey === true : event.ctrlKey === true;
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
  // Every pointer event carries the LIVE modifier state, so each one re-derives
  // `armed` in BOTH directions. Raising it on a pointer event but lowering it
  // only on the keyup edge strands the hold whenever that keyup is delivered
  // somewhere else — an OS app switch, a swallowed key — and a hold must never
  // outlive the level that justifies it.
  const onPointerModifiers = (event: MouseEvent): void => {
    setArmed(modifierHeld(event));
  };
  const onPointerEnter = (event: MouseEvent): void => {
    pointerInside = true;
    onPointerModifiers(event);
    recomputeHold();
  };
  const onPointerLeave = (): void => {
    pointerInside = false;
    recomputeHold();
  };

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
    onPointerModifiers(event);
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
    if (!target || !isTerminalLinkActivationGesture(event, linkActivationArmed)) {
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

  let interactionListenersAttached = false;
  const attachInteractionListeners = (): void => {
    if (interactionListenersAttached) return;
    interactionListenersAttached = true;
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("blur", releaseInteraction);
    container.addEventListener("mouseover", onOver);
    container.addEventListener("mouseout", onOut);
    container.addEventListener("mouseenter", onPointerEnter);
    container.addEventListener("mouseleave", onPointerLeave);
    container.addEventListener("mousemove", onPointerModifiers);
    container.addEventListener("mousedown", onPointerModifiers);
    container.addEventListener("click", onClick);
  };
  const detachInteractionListeners = (): void => {
    if (!interactionListenersAttached) return;
    interactionListenersAttached = false;
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
  };
  const setActive = (nextActive: boolean): void => {
    if (disposed || nextActive === active) return;
    active = nextActive;
    if (!active) {
      detachInteractionListeners();
      releaseInteraction();
      scanner.setActive(false);
      return;
    }
    scanner.setActive(true);
    attachInteractionListeners();
  };

  if (active) attachInteractionListeners();

  const dispose = (): void => {
    if (disposed) return;
    setActive(false);
    disposed = true;
    scanner.dispose();
    hintElement?.remove();
    hintElement = null;
  };
  return { setActive, releaseInteraction, openLink, describeLink, dispose };
}
