// Shared right-click context-menu primitives. BOTH the terminal-pane menu
// (TerminalContextMenu) and the sidebar-row menu (SessionRowContextMenu) build
// from these so they are visually identical — same surface (bg / border /
// radius / shadow / padding / font), same item shape, same hover, same
// separators. Neither menu styles items inline (that's what drifted them apart
// + killed the hover, see sidebar.css:1741 `.df-menu-item:hover`).
//
// Desktop floating menus only — the terminal menu's mobile bottom-sheet stays
// in its own file (different interaction model).

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
		background: "var(--bg-elev-2)",
		border: "1px solid var(--border-strong)",
		"border-radius": "var(--md-shape-sm)",
		"box-shadow": "var(--md-elev-3)",
		padding: "4px",
		"user-select": "none",
		color: "var(--text-hi)",
		"font-size": "var(--md-body-s-size)",
	};
}

export function CtxMenuSeparator() {
	return (
		<div
			style={{
				height: "1px",
				background: "var(--border-strong)",
				margin: "4px 0",
			}}
		/>
	);
}

/** One menu row. `class="df-menu-item"` carries the hover highlight from
 *  sidebar.css — do NOT set an inline background here (it would override it).
 *  `disabled` greys the row + swallows the click (used when an action has no
 *  valid target, e.g. screen-share/finder on a worker with no reachable_addr —
 *  synthesizing a host from the label produces an unresolvable vnc:// URL). */
export function CtxMenuItem(props: {
	testid: string;
	onClick: (e: MouseEvent) => void;
	danger?: boolean;
	disabled?: boolean;
	title?: string;
	children: JSX.Element;
}) {
	return (
		<div
			data-testid={props.testid}
			class="df-menu-item"
			role="menuitem"
			aria-disabled={props.disabled ? "true" : undefined}
			title={props.title}
			onClick={(e) => {
				if (props.disabled) {
					e.stopPropagation();
					return;
				}
				props.onClick(e);
			}}
			style={{
				padding: "6px 10px",
				"border-radius": "var(--md-shape-xs)",
				cursor: props.disabled ? "default" : "pointer",
				opacity: props.disabled ? "0.4" : "1",
				color: props.danger ? "var(--color-err)" : "var(--text-hi)",
			}}
		>
			{props.children}
		</div>
	);
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
 *  owning reactive scope's lifetime. A click closes UNLESS it landed inside an
 *  element produced by `within` (the trigger button, the menu itself) — item
 *  clicks close explicitly instead, keeping ordering deterministic against
 *  Solid's delegated events. */
export function trackFloatingMenuDismiss(opts: {
	onClose(): void;
	within?: Array<() => Element | undefined | null>;
}): void {
	const onDocClick = (e: MouseEvent) => {
		const t = e.target as Node | null;
		if (t) for (const el of opts.within ?? []) if (el()?.contains(t)) return;
		opts.onClose();
	};
	const onEsc = (e: KeyboardEvent) => {
		if (e.key === "Escape") opts.onClose();
	};
	document.addEventListener("click", onDocClick);
	document.addEventListener("keydown", onEsc);
	onCleanup(() => {
		document.removeEventListener("click", onDocClick);
		document.removeEventListener("keydown", onEsc);
	});
}
