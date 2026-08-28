// DOM wiring for terminal mouse/touch forwarding — the counterpart to the pure
// encoder in terminalMouse.ts. This module owns the listeners, the hit test and
// the press/touch state machine; terminalMouse.ts owns the decision and the
// bytes.
//
// Forward pointer and touch gestures ONLY to an application that asked for
// mouse reporting (frame.mouseTracking, DECSET 1000/1002 read off the core).
// Alt-screen occupancy is not that question: vim/less/man occupy it without
// requesting the mouse, and forwarding to them swallowed the click with no
// native fallback. The user toggle survives as the override for the opposite
// case — native selection inside an app that DOES want the mouse.
// Shift or Alt bypasses forwarding for one gesture (see terminalMouse.ts).
//
// Listener ownership is split three ways, matching the pane's lifetimes:
//   * pane-local press/touch listeners attach for the mount (attach → dispose)
//   * the drag continuation lives on window, attached only while the pane is
//     visible, so parked tabs don't multiply document event work
//   * wheel/touchmove re-attach on every tracking-mode flip (passivity swap)

import { createEffect, onCleanup, type Accessor } from "solid-js";
import type { MouseTracking } from "@roost/shared/cell";
import { mouseForwardEnabled } from "./mouseForwardPref.ts";
import {
	cellFromPoint,
	terminalMouseReport,
	type MouseGesture,
} from "./terminalMouse.ts";
import type { CellGridRenderer } from "./cellRenderer.ts";

export interface TerminalMouseForwardingDeps {
	/** The pane's scroll container — every pane-local listener binds here. */
	display: HTMLDivElement;
	/** Tracking mode off the newest accepted cell frame. A SIGNAL, not a plain
	 *  read: the wheel/touchmove listener passivity is keyed on it. */
	mouseTracking: Accessor<MouseTracking>;
	/** Hand the encoded report to this session's input lane. */
	sendBytes: (bytes: Uint8Array) => void;
	getRenderer: () => CellGridRenderer | null;
	/** Whether the frame asked for SGR-1006 rather than legacy X10 encoding. */
	getMouseSgr: () => boolean;
	/** Last measured cell box in px; zero before the pane has layout. Two
	 *  accessors, not one object: `cellOf` runs on every forwarded mousemove. */
	getCellW: () => number;
	getCellH: () => number;
	/** Re-probe the cell box; true when it produced a usable measurement. */
	measureCell: () => boolean;
}

export interface TerminalMouseForwarding {
	/** Drag continuation. The pane attaches these to window only while it is
	 *  visible, and removes them on the same transition. */
	onWindowMouseMove(ev: MouseEvent): void;
	onWindowMouseUp(ev: MouseEvent): void;
	/** Wheel/touchmove passivity swap. MUST be called inside the pane's reactive
	 *  owner so its listeners are removed with the pane. */
	bindWheelAndTouchMove(): void;
	/** Remove the pane-local press/touch listeners on unmount. */
	dispose(): void;
}

export function attachTerminalMouseForwarding(
	deps: TerminalMouseForwardingDeps,
): TerminalMouseForwarding {
	const {
		display,
		mouseTracking,
		sendBytes,
		getRenderer,
		getMouseSgr,
		getCellW,
		getCellH,
		measureCell,
	} = deps;
	const forwardActive = () => mouseForwardEnabled() && mouseTracking() !== 0;
	// Reader intent must only precede a native gesture that can actually move the
	// display. Entering reader mode at a clamped edge (or with no overflow)
	// freezes live painting even though the browser has no scroll to perform.
	// `deltaY` is expressed as the resulting scrollTop direction: negative moves
	// toward history, positive moves toward the live tail.
	const enterReadingForNativeScroll = (
		reason: "wheel" | "touch",
		deltaY: number,
	): void => {
		const maxScrollTop = Math.max(0, display.scrollHeight - display.clientHeight);
		const canMove =
			deltaY < 0 ? display.scrollTop > 0 : display.scrollTop < maxScrollTop;
		if (canMove) getRenderer()?.enterReading(reason);
	};
	const report = (gesture: MouseGesture): boolean => {
		const bytes = terminalMouseReport(
			{ tracking: mouseTracking(), sgr: getMouseSgr() },
			gesture,
		);
		if (bytes === null) return false;
		sendBytes(bytes);
		return true;
	};
	// Hit-test against the PAINTED grid (renderer.viewportCellGeometry), never
	// the scroll container: the history spacer and the scrollback sheet sit
	// above .cell-viewport inside it, so a container-relative row was off by
	// (painted history − scrollTop) — the reported "click above the target"
	// offset. The fallback only covers the pre-first-frame window (no frame,
	// no measurable row box); it still takes its origin from the viewport
	// element when the renderer exists, and clamps nothing because there is no
	// known grid to clamp to yet.
	const cellOf = (
		clientX: number,
		clientY: number,
	): { col: number; row: number } => {
		const geometry = getRenderer()?.viewportCellGeometry();
		if (geometry) return cellFromPoint(geometry, clientX, clientY);
		if ((getCellW() === 0 || getCellH() === 0) && !measureCell())
			return { col: 1, row: 1 };
		// predictionHost IS .cell-viewport — row 0's box, unlike display.
		const origin = (getRenderer()?.predictionHost ?? display).getBoundingClientRect();
		return {
			col: Math.max(1, 1 + Math.floor((clientX - origin.left) / getCellW())),
			row: Math.max(1, 1 + Math.floor((clientY - origin.top) / getCellH())),
		};
	};

	let pressedButton: number | null = null;
	let lastMotionCell: { col: number; row: number } | null = null;

	const onWheelForward = (ev: WheelEvent) => {
		if (ev.defaultPrevented || ev.deltaY === 0) return;
		if (!forwardActive()) {
			enterReadingForNativeScroll("wheel", ev.deltaY);
			return;
		}
		const { col, row } = cellOf(ev.clientX, ev.clientY);
		const forwarded = report({
			kind: ev.deltaY < 0 ? "wheelUp" : "wheelDown",
			col, row,
			shift: ev.shiftKey, alt: ev.altKey, ctrl: ev.ctrlKey, meta: ev.metaKey,
		});
		if (!forwarded) {
			enterReadingForNativeScroll("wheel", ev.deltaY);
			return;
		}
		ev.preventDefault();
	};
	const onMouseDownFwd = (ev: MouseEvent) => {
		if (ev.defaultPrevented) return;
		if (!forwardActive()) return;
		// Modified link clicks open locally through the native anchor; never
		// forward them to the worker PTY.
		if ((ev.target as HTMLElement | null)?.closest("a")) return;
		// Middle button is reserved for the deck's bring-to-front toggle
		// (TerminalDeck onDeckPointerDown) — never forwarded as a press.
		if (ev.button === 1) return;
		const { col, row } = cellOf(ev.clientX, ev.clientY);
		if (!report({
			kind: "press", button: ev.button, col, row,
			shift: ev.shiftKey, alt: ev.altKey, ctrl: ev.ctrlKey, meta: ev.metaKey,
		})) return;
		ev.preventDefault();
		pressedButton = ev.button;
		lastMotionCell = { col, row };
	};
	const onMouseMoveFwd = (ev: MouseEvent) => {
		if (ev.defaultPrevented) return;
		if (pressedButton === null || !forwardActive()) return;
		// The application owns this drag — it received the press — so the browser
		// must not start a native selection under it even in mode 1000, which
		// reports no motion.
		ev.preventDefault();
		const { col, row } = cellOf(ev.clientX, ev.clientY);
		if (
			lastMotionCell &&
			lastMotionCell.col === col &&
			lastMotionCell.row === row
		)
			return;
		lastMotionCell = { col, row };
		report({
			kind: "motion", button: pressedButton, held: true, col, row,
			ctrl: ev.ctrlKey, meta: ev.metaKey,
		});
	};
	const onMouseUpFwd = (ev: MouseEvent) => {
		if (ev.defaultPrevented) {
			pressedButton = null;
			lastMotionCell = null;
			return;
		}
		if (pressedButton === null) return;
		const button = pressedButton;
		pressedButton = null;
		lastMotionCell = null;
		if (!forwardActive()) return;
		ev.preventDefault();
		const { col, row } = cellOf(ev.clientX, ev.clientY);
		report({
			kind: "release", button, col, row,
			ctrl: ev.ctrlKey, meta: ev.metaKey,
		});
	};

	// A touch becomes intent only after one cell-height of vertical travel.
	// Below that shared native/forwarding threshold a tap changes no state.
	let touchY: number | null = null;
	let touchForwarding = false;
	let touchCol = 1,
		touchRow = 1;
	const onTouchStart = (ev: TouchEvent) => {
		touchY = null;
		touchForwarding = false;
		if (ev.defaultPrevented || ev.touches.length !== 1) return;
		const touch = ev.touches[0]!;
		touchY = touch.clientY;
		touchForwarding = forwardActive();
		if (!touchForwarding) return;
		const cell = cellOf(touch.clientX, touch.clientY);
		touchCol = cell.col;
		touchRow = cell.row;
	};
	const onTouchMove = (ev: TouchEvent) => {
		if (ev.defaultPrevented || touchY === null || ev.touches.length !== 1) return;
		const y = ev.touches[0]!.clientY;
		const step = getCellH() || 18;
		let dy = y - touchY;
		if (Math.abs(dy) < step) return;
		if (!touchForwarding || !forwardActive()) {
			touchY = y;
			enterReadingForNativeScroll("touch", -dy);
			return;
		}
		while (Math.abs(dy) >= step) {
			const up = dy > 0; // finger moved down → scroll up (history)
			report({ kind: up ? "wheelUp" : "wheelDown", col: touchCol, row: touchRow });
			dy -= up ? step : -step;
		}
		touchY = y - dy; // carry sub-notch remainder
		ev.preventDefault(); // suppress native scroll only after forwarding
	};
	const onTouchEnd = () => {
		touchY = null;
		touchForwarding = false;
	};

	display.addEventListener("mousedown", onMouseDownFwd);
	display.addEventListener("touchstart", onTouchStart, { passive: true });
	display.addEventListener("touchend", onTouchEnd, { passive: true });
	display.addEventListener("touchcancel", onTouchEnd, { passive: true });

	return {
		onWindowMouseMove: onMouseMoveFwd,
		onWindowMouseUp: onMouseUpFwd,
		// Wheel/touchmove passivity: preventDefault (mouse forwarding) only ever
		// fires for an app that REQUESTED mouse reporting — outside that the
		// always-non-passive listeners disabled compositor fast-scroll on every
		// terminal, including every alt-screen pager that never asked for a mouse.
		// Swap on tracking-mode flips. Remove-then-add matters: `passive` isn't part
		// of listener identity, so re-adding the same fn without removing first is
		// silently ignored as a duplicate (the effect's onCleanup runs before each
		// re-run).
		bindWheelAndTouchMove(): void {
			createEffect(() => {
				const passive = mouseTracking() === 0;
				display.addEventListener("wheel", onWheelForward, { passive });
				display.addEventListener("touchmove", onTouchMove, { passive });
				onCleanup(() => {
					display.removeEventListener("wheel", onWheelForward);
					display.removeEventListener("touchmove", onTouchMove);
				});
			});
		},
		dispose(): void {
			display.removeEventListener("mousedown", onMouseDownFwd);
			display.removeEventListener("touchstart", onTouchStart);
			display.removeEventListener("touchend", onTouchEnd);
			display.removeEventListener("touchcancel", onTouchEnd);
		},
	};
}
