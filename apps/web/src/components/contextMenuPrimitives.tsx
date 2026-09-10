// Owns shared floating-menu chrome, anchoring, dismissal, and keyboard behavior.
// Terminal, sidebar, pane, arrange, and compact workspace menus compose it.
// It depends on Solid lifecycle plus the shared design-token menu CSS.
// Mobile bottom sheets remain separate; anchored menus may render above them.

import { onCleanup, type JSX } from "solid-js";

/** Canonical floating-menu surface. zIndex defaults to the terminal menu's 40;
 *  the sidebar-row menu passes 100 to sit above its click-away scrim (99). */
export function ctxMenuSurfaceStyle(
	x: number,
	y: number,
	zIndex = 40,
): JSX.CSSProperties {
	return {
		position: "fixed",
		left: `${x}px`,
		top: `${y}px`,
		"z-index": String(zIndex),
		"min-width": "180px",
		background: "var(--md-surface-container-high)",
		border: "1px solid var(--md-outline-variant)",
		"border-radius": "var(--md-shape-sm)",
		"box-shadow": "var(--md-elev-3)",
		padding: "var(--md-space-1)",
		"user-select": "none",
		color: "var(--text-hi)",
		"font-size": "var(--md-body-s-size)",
	};
}

export function CtxMenuSeparator() {
	return (
		<div
			role="separator"
			style={{
				border: "0 solid var(--md-outline-variant)",
				"border-block-start-width": "var(--workbench-border-width)",
				margin: "var(--md-space-1) 0",
			}}
		/>
	);
}
/** One native, programmatically focusable menu row. Items stay out of the
 * sequential tab order; `disabled` makes unreachable actions unavailable to
 * both roving focus and pointer activation. */
export function CtxMenuItem(props: {
  testid: string;
  onClick: (e: MouseEvent) => void;
  danger?: boolean;
  disabled?: boolean;
  selected?: boolean;
  highlighted?: boolean;
  class?: string;
  onFocus?: (event: FocusEvent) => void;
  onMouseEnter?: (event: MouseEvent) => void;
  title?: string;
  children: JSX.Element;
}) {
  return (
    <button
      type="button"
      data-testid={props.testid}
      class={`df-menu-item${props.danger ? " df-menu-item--danger" : ""}${props.class ? ` ${props.class}` : ""}`}
      role="menuitem"
      tabIndex={-1}
      disabled={props.disabled}
      aria-disabled={props.disabled ? "true" : undefined}
      aria-current={props.selected ? "page" : undefined}
      data-selected={props.selected ? "true" : undefined}
      data-highlighted={props.highlighted ? "true" : undefined}
      title={props.title}
      onFocus={props.onFocus}
      onMouseEnter={props.onMouseEnter}
      onClick={props.onClick}
    >
      {props.children}
    </button>
  );
}

export type MenuFocusEdge = "first" | "last";

function enabledMenuItems(menuElement: HTMLElement | undefined): HTMLButtonElement[] {
	return Array.from(menuElement?.querySelectorAll<HTMLButtonElement>(
		'[role="menuitem"]:not(:disabled)',
	) ?? []);
}

/** Focus after the owning Show/Portal has mounted its menu subtree. A Portal
 * can miss the first microtask, so disconnected/missing items retry by frame. */
export function focusMenuEdge(
	menuElement: () => HTMLElement | undefined,
	edge: MenuFocusEdge,
): () => void {
	let cancelled = false;
	let animationFrame: number | null = null;
	let attempts = 0;
	const focusWhenMounted = () => {
		if (cancelled) return;
		const menu = menuElement();
		const items = menu?.isConnected ? enabledMenuItems(menu) : [];
		const target = items[edge === "first" ? 0 : items.length - 1];
		if (target) {
			target.focus();
			if (document.activeElement === target) return;
		}
		attempts++;
		if (attempts < 4) animationFrame = requestAnimationFrame(focusWhenMounted);
	};
	queueMicrotask(focusWhenMounted);
	return () => {
		cancelled = true;
		if (animationFrame !== null) cancelAnimationFrame(animationFrame);
	};
}

/** Shared roving focus and activation semantics for floating menus. */
export function handleMenuKeyboardNavigation(
	event: KeyboardEvent,
	menuElement: HTMLElement | undefined,
	onEscape: () => void,
	onTab: () => void,
): void {
	if (event.key === "Escape") {
		event.preventDefault();
		event.stopPropagation();
		onEscape();
		return;
	}
	if (event.key === "Tab") {
		// Let native sequential focus leave the programmatic menu item first.
		queueMicrotask(onTab);
		return;
	}
	const items = enabledMenuItems(menuElement);
	if (items.length === 0) return;
	const currentIndex = items.indexOf(document.activeElement as HTMLButtonElement);
	let targetIndex: number | null = null;
	if (event.key === "ArrowDown") {
		targetIndex = currentIndex < 0 ? 0 : (currentIndex + 1) % items.length;
	} else if (event.key === "ArrowUp") {
		targetIndex = currentIndex < 0
			? items.length - 1
			: (currentIndex - 1 + items.length) % items.length;
	} else if (event.key === "Home") {
		targetIndex = 0;
	} else if (event.key === "End") {
		targetIndex = items.length - 1;
	} else if (event.key === "Enter" || event.key === " ") {
		const activeItem = items[currentIndex];
		if (!activeItem) return;
		event.preventDefault();
		event.stopPropagation();
		activeItem.click();
		return;
	} else {
		return;
	}
	event.preventDefault();
	event.stopPropagation();
	if (targetIndex !== null) items[targetIndex]?.focus();
}

// ─── right-anchored menus ─────────────────────────────────────────────────────
// ArrangeMenu, MobileDeckBar's WorkspaceTabsMenu and PaneStrip's PaneTabList
// all anchor a floating menu UNDER a trigger button with their RIGHT edges
// aligned and dismiss on outside-click/Escape. The three copies drifted only
// in min-width and z-index; these primitives pin the shared geometry and the
// deterministic dismissal so a fourth menu can't fork them again.

export interface AnchoredMenuPos {
	right: number;
	y: number;
}

/** Menu position anchored below `btn`, right edges aligned. `right` is an
 *  offset from the viewport's right edge so a shrink-fit menu grows leftward
 *  and can never overflow the screen while its width tracks its content. */
export function anchoredMenuPosition(btn: Element): AnchoredMenuPos {
	const r = btn.getBoundingClientRect();
	return { right: Math.max(6, window.innerWidth - r.right), y: r.bottom + 4 };
}

/** Right-edge-anchored surface: ctxMenuSurfaceStyle with `left` deleted (a
 *  stale left + our right would both constrain the box into full width) and
 *  the trigger-derived `right` applied. `extra` wins for per-menu overrides
 *  (max-width, padding, layout) on top of the shared chrome. */
export function anchoredMenuSurfaceStyle(
  pos: AnchoredMenuPos,
  opts: { minWidth: string; zIndex?: number; extra?: JSX.CSSProperties },
): JSX.CSSProperties {
  const s: JSX.CSSProperties = {
    ...ctxMenuSurfaceStyle(0, pos.y, opts.zIndex),
    "min-width": opts.minWidth,
    right: `${pos.right}px`,
    ...opts.extra,
  };
  delete s.left;
  return s;
}

/** Outside-click + Escape dismissal for a floating menu, registered for the
 * owning reactive scope's lifetime. A click closes UNLESS it landed inside an
 * element produced by `within` (the trigger button, the menu itself) — item
 * clicks close explicitly instead, keeping ordering deterministic against
 * Solid's delegated events. */
export function trackFloatingMenuDismiss(opts: {
  onClose(): void;
  onEscape?(): void;
  within?: Array<() => Element | undefined | null>;
}): void {
  const onDocClick = (e: MouseEvent) => {
    const t = e.target as Node | null;
    if (t) for (const el of opts.within ?? []) if (el()?.contains(t)) return;
    opts.onClose();
  };
  const onEsc = (e: KeyboardEvent) => {
    if (e.key === "Escape") (opts.onEscape ?? opts.onClose)();
  };
  document.addEventListener("click", onDocClick);
  document.addEventListener("keydown", onEsc);
  onCleanup(() => {
    document.removeEventListener("click", onDocClick);
    document.removeEventListener("keydown", onEsc);
  });
}
