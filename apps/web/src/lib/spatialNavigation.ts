// Directional (D-pad) focus navigation for TV mode. Chromium's own spatial
// navigation is not guaranteed on TV browsers and the app ships none, so a
// remote's four arrows would otherwise never move DOM focus at all.
//
// Installed once from App.tsx onMount. Listens in the BUBBLE phase so it is
// strictly the last claimant on an arrow key: anything with its own arrow
// handling (the shortcut router, a Kobalte listbox, a scrolling .wterm) has
// already run and either consumed the key or left it alone.
// Depends on: lib/tvMode.ts, @roost/shared/diag.

import { diag } from "@roost/shared/diag";
import { tvModeActive } from "./tvMode.ts";

type Direction = "up" | "down" | "left" | "right";

const DIRECTION_BY_KEY: Record<string, Direction> = {
	ArrowUp: "up",
	ArrowDown: "down",
	ArrowLeft: "left",
	ArrowRight: "right",
};

const FOCUSABLE_SELECTOR = [
	"a[href]",
	"button:not([disabled])",
	"input:not([disabled])",
	"select:not([disabled])",
	"textarea:not([disabled])",
	'[tabindex]:not([tabindex="-1"])',
].join(",");

// Roving-focus surfaces own their own arrow handling (contextMenuPrimitives).
const ROVING_ROLES = '[role="menu"],[role="listbox"],[role="combobox"]';

// Off-axis drift is penalised twice as hard as distance along the travel axis,
// so a control almost straight ahead beats a nearer one far to the side.
const CROSS_AXIS_WEIGHT = 2;

/** Nearest focusable in `direction`, or null when nothing lies that way.
 *  Exported for the unit test — ordinary callers use installSpatialNavigation. */
export function _bestCandidateInDirection(
	from: DOMRect,
	candidates: readonly HTMLElement[],
	direction: Direction,
): HTMLElement | null {
	const fromCenterX = from.left + from.width / 2;
	const fromCenterY = from.top + from.height / 2;
	let best: HTMLElement | null = null;
	let bestScore = Infinity;
	for (const candidate of candidates) {
		const rect = candidate.getBoundingClientRect();
		const centerX = rect.left + rect.width / 2;
		const centerY = rect.top + rect.height / 2;
		let primary: number;
		let cross: number;
		switch (direction) {
			case "up":
				primary = from.top - centerY;
				cross = Math.abs(centerX - fromCenterX);
				break;
			case "down":
				primary = centerY - from.bottom;
				cross = Math.abs(centerX - fromCenterX);
				break;
			case "left":
				primary = from.left - centerX;
				cross = Math.abs(centerY - fromCenterY);
				break;
			case "right":
				primary = centerX - from.right;
				cross = Math.abs(centerY - fromCenterY);
				break;
		}
		if (primary <= 0) continue;
		const score = primary + CROSS_AXIS_WEIGHT * cross;
		if (score < bestScore) {
			bestScore = score;
			best = candidate;
		}
	}
	return best;
}

let installed = false;

export function installSpatialNavigation(): () => void {
	if (installed)
		return () => {
			/* already installed */
		};
	installed = true;
	// BUBBLE phase, deliberately. Directional navigation is the "nobody claimed
	// this arrow" fallback, so it must run after every target and bubble handler
	// has had its turn — a Kobalte select/listbox, a slider, a roving tab strip.
	// In capture phase `defaultPrevented` can only reflect the one earlier
	// window-capture listener, so those components would be hijacked before they
	// ever saw the key.
	window.addEventListener("keydown", handleDirectionalKeydown);
	return () => {
		installed = false;
		window.removeEventListener("keydown", handleDirectionalKeydown);
	};
}

function handleDirectionalKeydown(event: KeyboardEvent): void {
	if (!tvModeActive()) return;
	if (event.defaultPrevented) return;
	if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
	const direction = DIRECTION_BY_KEY[event.key];
	if (!direction) return;

	const active = document.activeElement as HTMLElement | null;
	if (active) {
		const tag = active.tagName;
		if (tag === "INPUT" || tag === "TEXTAREA" || active.isContentEditable) return;
		if (active.closest?.(ROVING_ROLES)) return;
		if (scrollBoxCanStillMove(active, direction)) return;
	}

	const candidates = collectCandidates(active);
	if (candidates.length === 0) return;
	const target = pickTarget(active, candidates, direction);
	if (!target) return;

	event.preventDefault();
	target.focus();
	target.scrollIntoView({ block: "nearest", inline: "nearest" });
	diag("tv.nav", {
		key: event.key,
		to: target.dataset.testid ?? target.id ?? target.tagName,
	});
}

/** The focused terminal scroll box keeps ↑/↓ for the browser's own scrolling
 *  until it is clamped; at the edge, focus leaves the pane instead of dead-
 *  ending. Mirrors the canMove edge guard in terminalMouseForwarding.ts. */
function scrollBoxCanStillMove(element: HTMLElement, direction: Direction): boolean {
	if (direction !== "up" && direction !== "down") return false;
	if (!element.matches?.(".wterm")) return false;
	return direction === "up"
		? element.scrollTop > 0
		: element.scrollTop < element.scrollHeight - element.clientHeight;
}

function collectCandidates(active: HTMLElement | null): HTMLElement[] {
	const out: HTMLElement[] = [];
	for (const element of document.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)) {
		if (element === active) continue;
		if (element.getClientRects().length === 0) continue;
		if (element.closest('[inert],[aria-hidden="true"]')) continue;
		out.push(element);
	}
	return out;
}

/** With no origin geometry (focus sits on <body> after a route change) the
 *  first press must land somewhere deterministic: the topmost-leftmost control. */
function pickTarget(
	active: HTMLElement | null,
	candidates: readonly HTMLElement[],
	direction: Direction,
): HTMLElement | null {
	const from = active?.getBoundingClientRect();
	if (from && (from.width > 0 || from.height > 0))
		return _bestCandidateInDirection(from, candidates, direction);
	let best: HTMLElement | null = null;
	let bestTop = Infinity;
	let bestLeft = Infinity;
	for (const candidate of candidates) {
		const rect = candidate.getBoundingClientRect();
		if (rect.top < bestTop || (rect.top === bestTop && rect.left < bestLeft)) {
			bestTop = rect.top;
			bestLeft = rect.left;
			best = candidate;
		}
	}
	return best;
}
