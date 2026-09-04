// Owns cell measurement and debounced terminal viewport publication.
// CellTerminal passes the canonical viewActive accessor used by every controller.
// Lifecycle decides when to publish; this module performs the state transition.
// Runtime supplies the mounted display, view lease, and shared cell dimensions.

import type { Accessor } from "solid-js";
import { diag } from "@roost/shared/diag";
import { isPageVisible } from "../lib/pageVisible.ts";
import type { CellTerminalRuntime } from "./cell-terminal-runtime.ts";
import type { CellTerminalPresentation } from "./cell-terminal-presentation.ts";

const VIEWPORT_DEBOUNCE_MS = 150;

export interface CellTerminalViewport {
	readonly viewActive: Accessor<boolean>;
	measureCell(): boolean;
	measureViewport(): { cols: number; rows: number } | null;
	shouldPublishActive(): boolean;
	publishInactive(): void;
	parkView(): void;
	publishViewport(): boolean;
	scheduleViewport(): void;
	cancelScheduled(): void;
	publishViewportNow(): boolean;
}

export function createCellTerminalViewport(
	runtime: CellTerminalRuntime,
	presentation: CellTerminalPresentation,
	pending: Accessor<boolean>,
	viewActive: Accessor<boolean>,
): CellTerminalViewport {
	let viewportTimer: ReturnType<typeof setTimeout> | null = null;
	let viewportCandidate: { cols: number; rows: number } | null = null;
	let unmeasuredFrame = 0;
	let unmeasuredRetryUsed = false;

	const measureCell = (): boolean => {
		const display = runtime.display();
		if (!display) return false;
		const probe = document.createElement("span");
		probe.className = "cell-row";
		probe.style.position = "absolute";
		probe.style.visibility = "hidden";
		probe.style.whiteSpace = "pre";
		probe.textContent = "0".repeat(10);
		display.appendChild(probe);
		const rect = probe.getBoundingClientRect();
		display.removeChild(probe);
		if (rect.width === 0 || rect.height === 0) return false;
		runtime.cellWidth = rect.width / 10;
		runtime.cellHeight = rect.height;
		return true;
	};
	const measureViewport = (): { cols: number; rows: number } | null => {
		const display = runtime.display();
		if (!display) return null;
		if (
			(runtime.cellWidth === 0 || runtime.cellHeight === 0)
			&& !measureCell()
		) return null;
		const styles = getComputedStyle(display);
		const paddingLeft = Number.parseFloat(styles.paddingLeft) || 0;
		const paddingRight = Number.parseFloat(styles.paddingRight) || 0;
		const paddingTop = Number.parseFloat(styles.paddingTop) || 0;
		const paddingBottom = Number.parseFloat(styles.paddingBottom) || 0;
		const usableWidth = display.clientWidth - paddingLeft - paddingRight;
		const usableHeight = display.clientHeight - paddingTop - paddingBottom;
		const cols = Math.floor(usableWidth / runtime.cellWidth);
		const rows = Math.floor(usableHeight / runtime.cellHeight);
		return cols > 0 && rows > 0 ? { cols, rows } : null;
	};
	const shouldPublishActive = (): boolean =>
		!runtime.unmounted
		&& !pending()
		&& isPageVisible()
		&& viewActive();
	const cancelScheduled = (): void => {
		if (!viewportTimer) return;
		clearTimeout(viewportTimer);
		viewportTimer = null;
	};
	const publishInactive = (): void => {
		presentation.clearFrameActivity();
		presentation.clearCursorBlink();
		presentation.setViewportLiveReady(false);
		runtime.backfill?.suspend();
		cancelScheduled();
		viewportCandidate = null;
		if (unmeasuredFrame !== 0) {
			cancelAnimationFrame(unmeasuredFrame);
			unmeasuredFrame = 0;
		}
		unmeasuredRetryUsed = false;
		runtime.view?.setInactive();
	};
	const parkView = (): void => {
		presentation.releasePaintHolds();
		publishInactive();
	};
	const retryUnmeasuredViewport = (): void => {
		if (unmeasuredFrame !== 0 || unmeasuredRetryUsed) return;
		unmeasuredRetryUsed = true;
		unmeasuredFrame = requestAnimationFrame(() => {
			unmeasuredFrame = 0;
			if (shouldPublishActive()) publishViewport();
		});
	};
	const publishViewport = (): boolean => {
		const display = runtime.display();
		const view = runtime.view;
		if (!display || !view) return false;
		if (!shouldPublishActive()) {
			parkView();
			return false;
		}
		const measured = measureViewport();
		if (!measured) {
			// A lifecycle-active 0×0 box is transiently unmeasured. Keep the
			// last positive lease instead of turning layout jitter into a leave.
			retryUnmeasuredViewport();
			return false;
		}
		unmeasuredRetryUsed = false;
		view.setViewport(measured);
		diag("terminal.view_publish", {
			sid: runtime.sessionId,
			view_id: view.viewId,
			cols: measured.cols,
			rows: measured.rows,
		});
		return true;
	};
	const scheduleViewport = (): void => {
		cancelScheduled();
		viewportTimer = setTimeout(() => {
			viewportTimer = null;
			if (!shouldPublishActive()) {
				viewportCandidate = null;
				publishViewport();
				return;
			}
			const measured = measureViewport();
			if (!measured) {
				viewportCandidate = null;
				publishViewport();
				return;
			}
			if (
				viewportCandidate?.cols !== measured.cols
				|| viewportCandidate.rows !== measured.rows
			) {
				viewportCandidate = measured;
				scheduleViewport();
				return;
			}
			viewportCandidate = null;
			publishViewport();
		}, VIEWPORT_DEBOUNCE_MS);
	};
	const publishViewportNow = (): boolean => {
		cancelScheduled();
		viewportCandidate = null;
		return publishViewport();
	};

	return {
		viewActive,
		measureCell,
		measureViewport,
		shouldPublishActive,
		publishInactive,
		parkView,
		publishViewport,
		scheduleViewport,
		cancelScheduled,
		publishViewportNow,
	};
}
