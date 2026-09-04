// Mounts renderer, history backfill, prediction, input control, and stream feeds.
// CellTerminal creates the view first, then gives this owner the canonical pane
// contracts. Stream disconnection stays separate from imperative disposal so
// teardown can stop inbound work before releasing renderer resources.

import { createEffect, type Accessor, type Setter } from "solid-js";
import type { MouseTracking } from "@roost/shared/cell";
import { diag, isDiagEnabled } from "@roost/shared/diag";
import { coordClient } from "../connect.ts";
import { CellGridRenderer } from "../lib/cellRenderer.ts";
import { registerCursorPoll } from "../lib/cursorPollTicker.ts";
import { markPhase } from "../lib/diag.ts";
import { recordInputRtt } from "../lib/leakWatch.ts";
import { PredictiveEcho } from "../lib/predictiveEcho.ts";
import { predictMode } from "../lib/predictPref.ts";
import { createScrollbackBackfill } from "../lib/scrollbackBackfill.ts";
import { TerminalInputController } from "../lib/terminalInputController.ts";
import { registerRenderer } from "../lib/terminalPreview.ts";
import { registerUserTerminalInput } from "../lib/userTerminalInput.ts";
import { isPageVisible } from "../lib/pageVisible.ts";
import { isTouchDevice } from "../lib/windowSizeClass.ts";
import { publishMountedSpawnMeasurement } from "../store/optimisticSpawn.ts";
import { registerPresenceHandler } from "../store/sync.ts";
import { consumeLastInputSendTs } from "../ws/sync-outbound.ts";
import { activeComposeSessionId } from "./TerminalComposeButton.tsx";
import { sessionTitle } from "../lib/sessionTitle.ts";
import type { CellTerminalInput } from "./cell-terminal-input.ts";
import type { CellTerminalPresentation } from "./cell-terminal-presentation.ts";
import type { CellTerminalProps } from "./cell-terminal-types.ts";
import type { CellTerminalRuntime } from "./cell-terminal-runtime.ts";
import type { CellTerminalViewport } from "./cell-terminal-viewport.ts";

interface CellTerminalRendererSignals {
	pending: Accessor<boolean>;
	setAltScreen: Setter<boolean>;
	setMouseTracking: Setter<MouseTracking>;
}

export interface CellTerminalRendererMount {
	disconnectStream(): void;
	disposeResources(): void;
}

export function _terminalForegroundWorkAllowed(
	viewport: Pick<CellTerminalViewport, "viewActive">,
): boolean {
	return viewport.viewActive() && isPageVisible();
}

export function mountCellTerminalRenderer(
	props: CellTerminalProps,
	runtime: CellTerminalRuntime,
	input: CellTerminalInput,
	presentation: CellTerminalPresentation,
	viewport: CellTerminalViewport,
	signals: CellTerminalRendererSignals,
): CellTerminalRendererMount {
	const display = runtime.display();
	const view = runtime.view;
	if (!display || !view) {
		throw new Error("terminal renderer mounted without display or view");
	}

	const renderer = new CellGridRenderer(
		display,
		() => presentation.setHasReconciledFrame(true),
		presentation.refreshTerminalPresentation,
	);
	runtime.renderer = renderer;
	presentation.refreshCursorBlink();
	const backfill = createScrollbackBackfill({
		sessionId: runtime.sessionId,
		renderer: () => runtime.renderer,
		active: () => viewport.viewActive() && isPageVisible(),
	});
	runtime.backfill = backfill;
	const unregisterUserInput = registerUserTerminalInput(
		runtime.sessionId,
		presentation.prepareLiveInteraction,
	);
	const onScroll = (): void => {
		const currentRenderer = runtime.renderer;
		if (!currentRenderer) return;
		if (!viewport.viewActive() || !isPageVisible()) {
			presentation.notifyBackfill(currentRenderer.prepareLiveInteraction());
			return;
		}
		presentation.notifyBackfill(currentRenderer.handleScroll());
		if (!currentRenderer.atBottom() && currentRenderer.nearHistoryTop()) {
			backfill.onUserScrollUp();
		}
	};
	display.addEventListener("scroll", onScroll, { passive: true });
	const detachPointerGestureGuard = presentation.attachPointerGestureGuard();

	let lastCursorRow = 0;
	let lastCursorCol = 0;
	const unregisterPreview = registerRenderer(
		runtime.sessionId,
		renderer,
		() => {
			let cssVisible: boolean | null = null;
			const currentDisplay = runtime.display();
			if (currentDisplay?.isConnected) {
				const rect = currentDisplay.getBoundingClientRect();
				cssVisible = rect.width > 0 && rect.height > 0;
				for (
					let node: HTMLElement | null = currentDisplay;
					cssVisible && node;
					node = node.parentElement
				) {
					const style = getComputedStyle(node);
					if (
						style.display === "none"
						|| style.visibility === "hidden"
						|| style.visibility === "collapse"
						|| Number.parseFloat(style.opacity) === 0
						|| style.contentVisibility === "hidden"
					) cssVisible = false;
				}
			}
			return {
				handler_canonical: runtime.renderer?.canonicalEpochSeq()
					?? { grid_epoch: null, seq: null },
				slot: {
					connected: currentDisplay?.isConnected === true,
					in_layout: props.inLayout ?? null,
					surface_active: props.surfaceActive,
					css_visible: cssVisible,
				},
				visibility: {
					document_visible: document.visibilityState === "visible",
					page_visible: isPageVisible(),
				},
			};
		},
	);
	const ghostMap = new Map<string, { x: number; y: number; label?: string }>();
	const unsubscribeRenderer = view.subscribeRenderer(renderer, ({ frame }) => {
		const diagnosticsEnabled = isDiagEnabled();
		const frameArrivedAt = diagnosticsEnabled ? performance.now() : 0;
		const sentAt = consumeLastInputSendTs(runtime.sessionId);
		if (sentAt !== undefined) {
			const roundTripMs = performance.now() - sentAt;
			recordInputRtt(roundTripMs);
			if (roundTripMs > 0 && roundTripMs < 5000) {
				diag("echo.frame_rtt", {
					sid: runtime.sessionId,
					rtt_ms: roundTripMs,
				});
			}
		}
		if (diagnosticsEnabled) {
			diag("cell.apply", {
				sid: runtime.sessionId,
				stream_id: frame.streamId,
				seq: frame.seq,
				full: frame.full,
				vp_rows: frame.viewportRows.length,
				cursor_vis: frame.cursorVisible,
				cursor_row: frame.cursorRow,
				cursor_col: frame.cursorCol,
			});
		}
		signals.setAltScreen(frame.altScreen);
		if (runtime.revealStartedAt !== 0) {
			diag("cell.reveal", {
				sid: runtime.sessionId,
				ms: Math.round(performance.now() - runtime.revealStartedAt),
				full: frame.full,
			});
			runtime.revealStartedAt = 0;
		}
		if (
			diagnosticsEnabled
			&& isPageVisible()
			&& props.inLayout === true
			&& props.surfaceActive
		) {
			requestAnimationFrame(() => requestAnimationFrame(() => {
				const canonical = runtime.renderer?.canonicalEpochSeq()
					?? { grid_epoch: null, seq: null };
				const reconciled = runtime.renderer?.reconciledEpochSeq()
					?? { grid_epoch: null, seq: null };
				diag("cell.dom_reconcile_opportunity", {
					sid: runtime.sessionId,
					dur_ms: performance.now() - frameArrivedAt,
					canonical_epoch: canonical.grid_epoch,
					canonical_seq: canonical.seq,
					reconciled_epoch: reconciled.grid_epoch,
					reconciled_seq: reconciled.seq,
					block_reason: runtime.renderer?.reconcileBlockReason() ?? null,
				});
			}));
		}
		runtime.frameCursorKeysApplication = frame.cursorKeysApp;
		presentation.noteFrameActivity(frame);
		runtime.frameBracketedPaste = frame.bracketedPaste;
		runtime.frameMouseSgr = frame.mouseSgr;
		runtime.frameFocusEvents = frame.focusEvents;
		signals.setMouseTracking(frame.mouseTracking);
		lastCursorRow = frame.cursorRow;
		lastCursorCol = frame.cursorCol;
		runtime.predictor?.onFrame(frame);
		if (frame.full) backfill.onFullFrame();
	});
	markPhase("terminal_mount", { sessionId: runtime.sessionId });

	let measurementFrame = 0;
	const publishSpawnMeasurement = (): boolean => {
		if (!signals.pending() || !viewport.viewActive() || !isPageVisible()) {
			return false;
		}
		const measured = viewport.measureViewport();
		if (!measured) return false;
		return publishMountedSpawnMeasurement(runtime.sessionId, measured);
	};
	if (!publishSpawnMeasurement()) {
		measurementFrame = requestAnimationFrame(() => {
			measurementFrame = 0;
			publishSpawnMeasurement();
		});
	}

	runtime.predictor = new PredictiveEcho(renderer.predictionHost, {
		mode: predictMode,
		sid: runtime.sessionId,
		onCursor: (column) => runtime.renderer?.setPredictedCursor(column),
	});
	createEffect(() => {
		predictMode();
		runtime.predictor?.refreshPreference();
	});
	if (
		import.meta.env.VITE_ROOST_SMOKE === "1"
		&& typeof localStorage !== "undefined"
		&& localStorage.getItem("roostSmoke") === "1"
	) {
		(window as Window & { __roostPredictDebug?: () => unknown }).__roostPredictDebug =
			() => runtime.predictor?._debug() ?? null;
	}

	runtime.inputController = new TerminalInputController(display, {
		cursorKeysApplication: () => runtime.frameCursorKeysApplication,
		focusEventsEnabled: () => runtime.frameFocusEvents,
		onData: input.sendControllerData,
		onPaste: (text, event) => {
			input.enqueueFileItems(event.clipboardData?.items);
			input.pasteText(text);
		},
		ariaLabel: `Terminal input — ${sessionTitle(props.session)}`,
	});
	if (
		_terminalForegroundWorkAllowed(viewport)
		&& props.focused === true
		&& !isTouchDevice()
		&& activeComposeSessionId() === null
	) {
		runtime.inputController.forceFocus();
		requestAnimationFrame(() => {
			if (
				!runtime.unmounted
				&& _terminalForegroundWorkAllowed(viewport)
				&& props.focused === true
				&& activeComposeSessionId() === null
			) runtime.inputController?.forceFocus();
		});
	}

	const unregisterPresence = registerPresenceHandler(runtime.sessionId, (message) => {
		const frame = message as {
			kind?: string;
			viewer_id?: string;
			cursor_col?: number;
			cursor_row?: number;
			label?: string;
		};
		if (frame.kind === "presence-delta" && typeof frame.viewer_id === "string") {
			ghostMap.set(frame.viewer_id, {
				x: frame.cursor_col ?? 0,
				y: frame.cursor_row ?? 0,
				label: frame.label ?? frame.viewer_id,
			});
			runtime.renderer?.setGhosts(ghostMap);
		} else if (
			frame.kind === "presence-leave"
			&& typeof frame.viewer_id === "string"
			&& ghostMap.delete(frame.viewer_id)
		) {
			runtime.renderer?.setGhosts(ghostMap);
		}
	});
	let lastSentRow = -1;
	let lastSentCol = -1;
	const releaseCursorPoll = registerCursorPoll(() => {
		if (!_terminalForegroundWorkAllowed(viewport)) return;
		if (lastCursorRow === lastSentRow && lastCursorCol === lastSentCol) return;
		lastSentRow = lastCursorRow;
		lastSentCol = lastCursorCol;
		void coordClient.sessionsCursorPos({
			sessionId: runtime.sessionId,
			col: lastCursorCol,
			row: lastCursorRow,
		});
	});

	let streamDisconnected = false;
	let resourcesDisposed = false;
	const disconnectStream = (): void => {
		if (streamDisconnected) return;
		streamDisconnected = true;
		unregisterUserInput();
		unsubscribeRenderer();
		unregisterPresence();
		releaseCursorPoll();
	};
	const disposeResources = (): void => {
		if (resourcesDisposed) return;
		resourcesDisposed = true;
		display.removeEventListener("scroll", onScroll);
		detachPointerGestureGuard();
		if (measurementFrame !== 0) cancelAnimationFrame(measurementFrame);
		backfill.dispose();
		if (runtime.backfill === backfill) runtime.backfill = null;
		runtime.predictor?.dispose();
		runtime.predictor = null;
		unregisterPreview();
		renderer.dispose();
		if (runtime.renderer === renderer) runtime.renderer = null;
		runtime.inputController?.destroy();
		runtime.inputController = null;
	};
	return { disconnectStream, disposeResources };
}
