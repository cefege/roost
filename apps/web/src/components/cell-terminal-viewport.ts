// Owns cell measurement and debounced terminal viewport publication.
// CellTerminal passes the canonical viewActive accessor used by every controller.
// Lifecycle decides when to publish and whether a withdraw is real; this module
// performs the state transition and owns the transient-layout-gap grace.
// Runtime supplies the mounted display, view lease, and shared cell dimensions.

import type { Accessor } from "solid-js";
import { diag } from "@roost/shared/diag";
import type { TerminalGeometry } from "@roost/shared/viewport";
import { isPageVisible } from "../lib/pageVisible.ts";
import {
	measureTerminalCellBox,
	terminalGeometryForBox,
} from "../lib/terminalCellGeometry.ts";
import type { CellTerminalRuntime } from "./cell-terminal-runtime.ts";
import type { CellTerminalPresentation } from "./cell-terminal-presentation.ts";

const VIEWPORT_DEBOUNCE_MS = 50;
const UNMEASURED_VIEWPORT_RETRY_MS = 100;
// Claims are debounced; withdraws caused by a transient layout gap get the
// symmetric treatment. Orders of magnitude below TERMINAL_VIEW_HEARTBEAT_MS
// (5 s) so a deferred withdraw can never put this view's lease at risk, and
// above a couple of layout ticks so one zero-sized deck frame is absorbed.
const LAYOUT_GAP_PARK_GRACE_MS = 250;

type UnmeasuredRetryPhase = "idle" | "frame" | "timer";
// A grace ends by absorbing the gap (the pane came back), by a real withdraw
// superseding it, or by expiring into the withdraw it delayed.
type LayoutGapGraceEnd = "absorbed" | "superseded";

export interface CellTerminalViewport {
	readonly viewActive: Accessor<boolean>;
	measureCell(): boolean;
	measureViewport(): TerminalGeometry | null;
	shouldPublishActive(): boolean;
	publishInactive(): void;
	parkView(): void;
	parkViewAfterLayoutGap(): void;
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
	let viewportTimer: Timer | null = null;
	let unmeasuredFrame: number | null = null;
	let unmeasuredRetryTimer: Timer | null = null;
	let unmeasuredRetryPhase: UnmeasuredRetryPhase = "idle";
	let layoutGapGraceTimer: Timer | null = null;

	const measureCell = (): boolean => {
		const display = runtime.display();
		if (!display) return false;
		const cell = measureTerminalCellBox(display);
		if (!cell) return false;
		runtime.cellWidth = cell.width;
		runtime.cellHeight = cell.height;
		return true;
	};
	const measureViewport = (): TerminalGeometry | null => {
		const display = runtime.display();
		if (!display) return null;
		if (
			(runtime.cellWidth === 0 || runtime.cellHeight === 0)
			&& !measureCell()
		) return null;
		return terminalGeometryForBox(display, {
			width: runtime.cellWidth,
			height: runtime.cellHeight,
		});
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
	const cancelUnmeasuredViewportRetry = (): void => {
		if (unmeasuredFrame !== null) {
			cancelAnimationFrame(unmeasuredFrame);
			unmeasuredFrame = null;
		}
		if (unmeasuredRetryTimer !== null) {
			clearTimeout(unmeasuredRetryTimer);
			unmeasuredRetryTimer = null;
		}
		unmeasuredRetryPhase = "idle";
	};
	const canRetryUnmeasuredViewport = (): boolean =>
		shouldPublishActive()
		&& runtime.display() !== undefined
		&& runtime.view !== null;
	const endLayoutGapGrace = (end: LayoutGapGraceEnd): void => {
		if (layoutGapGraceTimer === null) return;
		clearTimeout(layoutGapGraceTimer);
		layoutGapGraceTimer = null;
		diag("terminal.view_park_grace", {
			sid: runtime.sessionId,
			view_id: runtime.view?.viewId ?? null,
			phase: end,
		});
	};
	const publishInactive = (): void => {
		endLayoutGapGrace("superseded");
		presentation.clearFrameActivity();
		presentation.clearCursorBlink();
		runtime.backfill?.suspend();
		cancelScheduled();
		cancelUnmeasuredViewportRetry();
		runtime.view?.setInactive();
	};
	const parkView = (): void => {
		presentation.releasePaintHolds();
		publishInactive();
	};
	// Withdraw after a grace instead of now. Reserved for a transient layout
	// gap: a zero-sized deck drops every pane out of layout for a tick, and
	// withdrawing there re-mints the session's smallest common geometry for
	// every other viewer — a viewer that never left reframes everyone else.
	const parkViewAfterLayoutGap = (): void => {
		if (layoutGapGraceTimer !== null) return;
		diag("terminal.view_park_grace", {
			sid: runtime.sessionId,
			view_id: runtime.view?.viewId ?? null,
			phase: "start",
			grace_ms: LAYOUT_GAP_PARK_GRACE_MS,
		});
		layoutGapGraceTimer = setTimeout(() => {
			layoutGapGraceTimer = null;
			if (shouldPublishActive()) {
				publishViewport();
				return;
			}
			diag("terminal.view_park_grace", {
				sid: runtime.sessionId,
				view_id: runtime.view?.viewId ?? null,
				phase: "expired",
			});
			parkView();
		}, LAYOUT_GAP_PARK_GRACE_MS);
	};
	const retryUnmeasuredViewport = (): void => {
		if (!canRetryUnmeasuredViewport()) {
			cancelUnmeasuredViewportRetry();
			return;
		}
		if (unmeasuredRetryPhase === "idle") {
			unmeasuredRetryPhase = "frame";
			unmeasuredFrame = requestAnimationFrame(() => {
				unmeasuredFrame = null;
				if (!canRetryUnmeasuredViewport()) {
					cancelUnmeasuredViewportRetry();
					return;
				}
				publishViewport();
			});
			return;
		}
		if (
			unmeasuredRetryPhase !== "frame"
			|| unmeasuredFrame !== null
			|| unmeasuredRetryTimer !== null
		) return;
		unmeasuredRetryPhase = "timer";
		unmeasuredRetryTimer = setTimeout(() => {
			unmeasuredRetryTimer = null;
			if (!canRetryUnmeasuredViewport()) {
				cancelUnmeasuredViewportRetry();
				return;
			}
			if (!publishViewport()) unmeasuredRetryPhase = "idle";
		}, UNMEASURED_VIEWPORT_RETRY_MS);
	};
	const publishViewport = (): boolean => {
		const display = runtime.display();
		const view = runtime.view;
		if (!display || !view) {
			cancelUnmeasuredViewportRetry();
			return false;
		}
		if (!shouldPublishActive()) {
			// A deferred withdraw owns the transition until its grace ends:
			// parking here would turn the layout gap it absorbs into the
			// instant leave it exists to prevent. Every REAL cause parks
			// through parkView/publishInactive, which supersede the grace.
			if (layoutGapGraceTimer !== null) return false;
			parkView();
			return false;
		}
		endLayoutGapGrace("absorbed");
		const measured = measureViewport();
		if (!measured) {
			// A lifecycle-active 0×0 box is transiently unmeasured. Keep the
			// last positive lease instead of turning layout jitter into a leave.
			retryUnmeasuredViewport();
			return false;
		}
		cancelScheduled();
		cancelUnmeasuredViewportRetry();
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
			publishViewport();
		}, VIEWPORT_DEBOUNCE_MS);
	};
	const publishViewportNow = (): boolean => {
		cancelScheduled();
		return publishViewport();
	};

	return {
		viewActive,
		measureCell,
		measureViewport,
		shouldPublishActive,
		publishInactive,
		parkView,
		parkViewAfterLayoutGap,
		publishViewport,
		scheduleViewport,
		cancelScheduled,
		publishViewportNow,
	};
}
