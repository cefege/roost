// CellTerminal — the canonical cell-grid terminal pane. The worker owns VT
// parsing; this pane paints pre-rendered cells with CellGridRenderer and sends
// input through a synchronous pane-local textarea controller. No browser-side
// terminal core, hidden mirrored grid, WASM input oracle, or output reparse.
//
// Metadata alongside the cell stream lives here: remote cursor ghosts, OSC 8
// mappings, file links, predictive overlay and mouse SGR forwarding. Input and
// viewport control ride the generation-aware Sync v2 outbound owner.
import {
	onMount,
	onCleanup,
	createEffect,
	createMemo,
	createSignal,
	Show,
	getOwner,
	runWithOwner,
	on,
} from "solid-js";
import { TerminalInputController } from "../lib/terminalInputController.ts";
import { CellGridRenderer } from "../lib/cellRenderer.ts";
import { createScrollbackBackfill } from "../lib/scrollbackBackfill.ts";
import { PredictiveEcho } from "../lib/predictiveEcho.ts";
import { TerminalContextMenu } from "./TerminalContextMenu.tsx";
import { pickAndAttachFiles, enqueueAttachment } from "../lib/attachments.ts";
import type { TerminalContext } from "../lib/keytermContext.ts";
import {
	TerminalComposeButton,
	activeComposeSessionId,
} from "./TerminalComposeButton.tsx";
import { TerminalNavButtons } from "./TerminalNavButtons.tsx";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { attachTerminalMouseForwarding } from "../lib/terminalMouseForwarding.ts";
import type { MouseTracking } from "@roost/shared/cell";
import { isCompact, isTouchDevice } from "../lib/windowSizeClass.ts";
import { uiStore } from "../store/uiStore.ts";
import { FOCUS_OWNERS } from "../lib/focusOwners.ts";
import { attachTerminalLinks, type ResolveFile, type TerminalLinkAttachment } from "./terminal-links.ts";
import { downloadWorkerFileByHref } from "../lib/downloadWorkerFile.ts";
import { resolveWorkerPath, workerFileHref } from "../lib/nativePath.ts";
import { registerRenderer } from "../lib/terminalPreview.ts";
import { registerPresenceHandler } from "../store/sync.ts";
import {
	createTerminalView,
	type TerminalViewHandle,
	type TerminalViewHandleStatus,
} from "../store/terminal-stream.ts";
import {
	consumeLastInputSendTs,
	type InputAdmission,
} from "../ws/sync-outbound.ts";
import {
	registerUserTerminalInput,
	sendUserTerminalInput,
} from "../lib/userTerminalInput.ts";
import {
	recordInput,
	getInputText,
	clearInput,
} from "../lib/terminalInputHistory.ts";
import {
	buildPtyPayload,
	countLineBreaks,
	CR_BYTES,
	MULTILINE_PASTE_MIN_NEWLINES,
} from "../lib/ptyPaste.ts";
import { registerCursorPoll } from "../lib/cursorPollTicker.ts";
import { createTerminalSelectionGuard } from "../lib/terminalSelectionGuard.ts";
import { applyCtrlModifier, isAltGraphKey } from "../lib/terminalInput.ts";
import { coordClient } from "../connect.ts";
import { isResizeDragging, arrangeEpoch } from "../lib/resizeDrag.ts";
import { diag, isDiagEnabled, signal } from "@roost/shared/diag";
import { getSessionTraceId, markPhase, markPhaseOnce } from "../lib/diag.ts";
import { recordInputRtt } from "../lib/leakWatch.ts";
import { termFontSize } from "../lib/terminalFontPref.ts";
import { copyOnSelect } from "../lib/copyOnSelectPref.ts";
import { copyToClipboard } from "../lib/clipboard.ts";
import { Dialog, Button } from "./Settings/md/primitives.tsx";
import { sessionTitle } from "../lib/sessionTitle.ts";
import { createTerminalFind } from "../lib/terminalFindController.ts";
import { TerminalFindBar } from "./TerminalFindBar.tsx";
import type { ScrollbackBackfill } from "../lib/scrollbackBackfill.ts";
import type { Session } from "@roost/shared/wire";
import { useNavigate } from "@solidjs/router";
import { createOfflineWatch } from "../lib/offlineWatch.ts";
import { pageVisible, isPageVisible } from "../lib/pageVisible.ts";
import { newestOpenSessionForFolderKey } from "../store/selectors.ts";
import { folderKeyOf } from "../lib/folderKey.ts";
import { TerminalLoadingNotice, TerminalOfflineNotice, terminalViewportLoadingNotice } from "./TerminalOfflineNotice.tsx";
import {
	isPendingSpawn,
	publishMountedSpawnMeasurement,
} from "../store/optimisticSpawn.ts";
import { startAttachDiagnosis, type AttachDiagnosisHandle } from "../lib/attachDiagnosis.ts";
import type { BaselineProgress } from "../store/terminal-stream-types.ts";
import { createTerminalPresentationController } from "../lib/terminalPresentation.ts";
import { predictMode } from "../lib/predictPref.ts";
interface CellTerminalProps {
	session: Session;
	// In the current tiling layout (a visible pane's selected tab) → publish view
	// membership and render cells. Parked tabs remain mounted but inactive.
	inLayout?: boolean;
	// Owns the keyboard = the focused pane's selected tab. Only the focused pane
	// force-focuses and runs the document focus-recovery nets, so N live panes
	// don't fight for the keyboard.
	focused?: boolean;
	// The floated pane's selected tab (spotlight). Flipping this is an
	// intent-bearing resize → force an exact re-fit (see effect in onMount).
	spotlit?: boolean;
	// False while a non-terminal route overlays the persistent deck. Compact
	// accessories portal to <body>, so they must unmount explicitly rather than
	// relying on the deck host's visibility/pointer-events gate.
	surfaceVisible: boolean;
	// False when another desktop pane's spotlight scrim covers this surface.
	// Kept separate from surfaceVisible because the composer DOM stays mounted.
	surfaceActive: boolean;
}
const VIEWPORT_DEBOUNCE_MS = 150;
// Grace before a viewed-but-frameless pane is declared "not responding".
// Each retry republishes the current socket-bound view intent. A slow first
// baseline does not flash an offline notice, and the notice clears on paint.
const OFFLINE_GRACE_MS = 3000;
// Wire-dependent loading stages get a stall line only after this much
// continuous waiting; faster attaches never pay for a diagnosis poll.
const ATTACH_DIAGNOSIS_GRACE_MS = 2000;
export function CellTerminal(props: CellTerminalProps) {
	const sessionId = props.session.id;
	// Resolve terminal-emitted POSIX, drive, UNC, or relative paths through the
	// single worker-aware codec before building the reversible file route.
	const resolveFile: ResolveFile = (rawPath, line) => {
		const abs = resolveWorkerPath(
			props.session.worker_fp,
			props.session.cwd,
			rawPath,
		);
		return abs ? workerFileHref(props.session.worker_fp, abs, line) : null;
	};
	const attachSelectedFiles = () => pickAndAttachFiles(props.session);
	let displayRef: HTMLDivElement | undefined;
	const [ctrlArmed, setCtrlArmed] = createSignal(false);
	let renderer: CellGridRenderer | null = null;
	let linkAttachment: TerminalLinkAttachment | null = null;
	// The backfill controller is created inside onMount; the find controller and
	// interaction transitions need the mounted instance without owning its lifetime.
	let backfillRef: ScrollbackBackfill | null = null;
	let unregisterUserInput: (() => void) | null = null;
	// Native selection retention across renderer DOM replacement, plus the
	// live-interaction transitions that end a reader interval. Every ordering
	// constraint inside it is load-bearing — see lib/terminalSelectionGuard.ts.
	const {
		notifyBackfill,
		syncNativeSelectionHold,
		captureTerminalSelection,
		prepareLiveInteraction,
		releasePaintHolds,
	} = createTerminalSelectionGuard({
		getDisplay: () => displayRef,
		getRenderer: () => renderer,
		getBackfill: () => backfillRef,
		getLinkAttachment: () => linkAttachment,
	});
	let predictor: PredictiveEcho | null = null;
	let inputController: TerminalInputController | null = null;
	// These modes advance with every accepted canonical frame even when reader
	// intent keeps the reconciled DOM on an older frame.
	let frameCursorApp = false;
	let frameBracketed = false;
	let frameMouseSgr = false;
	let frameFocusEvents = false;
	let cellW = 0;
	let cellH = 0;
	// Zoom changes the cell box without resizing the containing element.
	let lastZoomPx = termFontSize();
	let resizeObs: ResizeObserver | null = null;
	let viewportTimer: ReturnType<typeof setTimeout> | null = null;
	let viewportCandidate: { cols: number; rows: number } | null = null;
	let unmeasuredRaf = 0;
	let view: TerminalViewHandle | null = null;
	// Reveal forensics: performance.now() at the visible-layout transition.
	let unmeasuredRetryUsed = false;
	let revealT0 = 0;
	const sendTerminalText = (text: string, submit = false): InputAdmission => {
		const payload = text.length === 0 ? new Uint8Array(0) : buildPtyPayload(text, frameBracketed);
		const bytes = submit ? new Uint8Array(payload.byteLength + CR_BYTES.byteLength) : payload;
		if (submit) {
			bytes.set(payload);
			bytes.set(CR_BYTES, payload.byteLength);
		}
		const admission = sendUserTerminalInput(sessionId, bytes, view?.viewId);
		if (!admission.accepted) {
			signal("input.drop_burst", {
				sid: sessionId,
				reason: admission.reason,
				cooldownKey: sessionId,
			});
			return admission;
		}
		if (text.length > 0) recordInput(sessionId, text);
		void admission.result.then((result) => {
			if (result.status === "accepted") return;
			signal("input.drop_burst", {
				sid: sessionId,
				reason: result.status === "ambiguous" ? "ambiguous" : result.reason,
				cooldownKey: sessionId,
			});
		});
		return admission;
	};
	const [pendingPaste, setPendingPaste] = createSignal<string | null>(null);
	const pendingPasteLines = () => {
		const text = pendingPaste();
		return text === null ? 0 : countLineBreaks(text) + 1;
	};
	/** The single entry point for every clipboard-originated paste: the ⌘⇧V
	 *  chord, the context menu, and a native paste we intercepted. */
	function pasteText(text: string): void {
		if (text.length === 0) return;
		if (countLineBreaks(text) >= MULTILINE_PASTE_MIN_NEWLINES && !frameBracketed) {
			setPendingPaste(text);
			return;
		}
		sendTerminalText(text);
	}
	const enqueueFileItems = (items: DataTransferItemList | null | undefined) => {
		if (!items) return;
		for (let i = 0; i < items.length; i++) {
			const item = items[i]!;
			if (item.kind !== "file") continue;
			const file = item.getAsFile();
			if (file) void enqueueAttachment(props.session, file);
		}
	};
	async function copySelectionToClipboard(): Promise<void> {
		const text = window.getSelection()?.toString() ?? "";
		if (!text) return;
		// Denial is non-fatal: the selection stays visible for manual copy.
		await copyToClipboard(text);
	}
	async function pasteFromClipboard(): Promise<void> {
		const text = await navigator.clipboard.readText().catch(() => "");
		pasteText(text);
	}
	// Find in scrollback. The controller owns the debounce, the single-flight
	// token and the highlight/jump wiring; the bar is presentation.
	const find = createTerminalFind({
		sessionId: props.session.id,
		renderer: () => renderer,
		backfill: () => backfillRef,
	});
	onCleanup(() => find.dispose());
	let unmounted = false;
	// Native browser scrolling owns the terminal position; the renderer classifies
	// its events and guards only its own scrollTop writes. This component never
	// writes scrollTop directly. The mode signal follows the newest accepted
	// canonical frame even while the reconciled DOM remains reader-frozen.
	const [altScreen, setAltScreen] = createSignal(false);
	// Mouse reporting the foreground app requested. A SIGNAL, not a plain local:
	// the wheel/touchmove listener passivity below is keyed on it, so it must
	// re-run when the app arms or drops tracking.
	const [mouseTracking, setMouseTracking] = createSignal<MouseTracking>(0);
	// True while this session is an optimistic placeholder (spawn RPC in flight).
	// For a non-optimistic session this is always false → every gate below is a
	// no-op, so mount behaviour is byte-identical to before.
	const pending = createMemo(() => isPendingSpawn(props.session.id));
	// A view is live-ready only after its active state is accepted and the
	// session replica has installed that stream's complete full baseline.
	const navigate = useNavigate();
	const [viewportLiveReady, setViewportLiveReady] = createSignal(false);
	const [viewStatus, setViewStatus] = createSignal<TerminalViewHandleStatus | null>(null);
	const [hasReconciledFrame, setHasReconciledFrame] = createSignal(false);
	const viewActiveFlag = createMemo(
		() => props.inLayout === true
			&& props.surfaceVisible
			&& props.surfaceActive,
	);
	const terminalPresentation = createTerminalPresentationController({
		active: viewActiveFlag,
		focused: () => props.focused === true,
		status: viewStatus,
		renderer: () => renderer,
	});
	const terminalPresentationState = terminalPresentation.state;
	const {
		clearFrameActivity,
		refreshTerminalPresentation,
		noteFrameActivity,
		clearCursorBlink,
		refreshCursorBlink,
	} = terminalPresentation;

	const [offline, setOffline] = createSignal(false);
	const retryOffline = () => view?.refresh();
	const offlineWatch = createOfflineWatch(OFFLINE_GRACE_MS, setOffline, () => {
		diag("cell.offline_retry", { sid: props.session.id });
		retryOffline();
	});
	createEffect(() =>
		offlineWatch.update(
			props.inLayout === true
				&& props.surfaceVisible
				&& props.surfaceActive
				&& isPageVisible(),
			hasReconciledFrame() && viewportLiveReady(),
		),
	);
	onCleanup(() => offlineWatch.dispose());
	const offlineSibling = () =>
		newestOpenSessionForFolderKey(folderKeyOf(props.session), props.session.id);
	const openOfflineSibling = () => {
		const sib = offlineSibling();
		if (sib) navigate(`/s/${sib.id}`);
	};
	const [attachProgress, setAttachProgress] = createSignal<BaselineProgress | null>(null);
	// The card speaks in received/total parts; the replica speaks in chunks.
	const loadingProgress = createMemo(() => {
		const progress = attachProgress();
		return progress === null
			? null
			: { received: progress.receivedChunks, total: progress.totalChunks };
	});
	const loadingNotice = createMemo(() => {
		if (
			props.inLayout !== true
			|| !props.surfaceVisible
			|| !props.surfaceActive
			|| !pageVisible()
			|| offline()
			|| (hasReconciledFrame() && viewportLiveReady())
		) return null;
		return terminalViewportLoadingNotice(pending(), viewStatus());
	});
	// Attach-progress meter + stall diagnosis. The meter rides the view's
	// chunk assembler; the diagnosis poll starts only after the loading card
	// has sat in a wire-dependent stage past ATTACH_DIAGNOSIS_GRACE_MS, and
	// dies on paint, park, or unmount via this effect's cleanup.
	const [stuckReason, setStuckReason] = createSignal<string | null>(null);
	let attachDiagnosis: AttachDiagnosisHandle | null = null;
	// Primitive stage on purpose: the notice object rebuilds on every
	// pending()/viewStatus() tick, and this effect must not restart its grace
	// window (or dispose an armed diagnosis) unless the stage itself changes.
	const loadingStage = createMemo(() => loadingNotice()?.stage ?? null);
	createEffect(() => {
		const waitingForWire = loadingStage() === "viewport" || loadingStage() === "frame";
		if (!waitingForWire) {
			setStuckReason(null);
			return;
		}
		const timer = setTimeout(() => {
			attachDiagnosis = startAttachDiagnosis(props.session.id, setStuckReason);
		}, ATTACH_DIAGNOSIS_GRACE_MS);
		onCleanup(() => {
			clearTimeout(timer);
			attachDiagnosis?.dispose();
			attachDiagnosis = null;
		});
	});
	// Measure one monospace cell in the display font (independent of rendered
	// content, so view activity can publish before the first frame arrives).
	function measureCell(): boolean {
		if (!displayRef) return false;
		const probe = document.createElement("span");
		probe.className = "cell-row";
		probe.style.position = "absolute";
		probe.style.visibility = "hidden";
		probe.style.whiteSpace = "pre";
		probe.textContent = "0".repeat(10);
		displayRef.appendChild(probe);
		const rect = probe.getBoundingClientRect();
		displayRef.removeChild(probe);
		if (rect.width === 0 || rect.height === 0) return false;
		cellW = rect.width / 10;
		cellH = rect.height;
		return true;
	}
	function measureViewport(): { cols: number; rows: number } | null {
		if (!displayRef) return null;
		if ((cellW === 0 || cellH === 0) && !measureCell()) return null;
		const cs = getComputedStyle(displayRef);
		const padL = parseFloat(cs.paddingLeft) || 0;
		const padR = parseFloat(cs.paddingRight) || 0;
		const padT = parseFloat(cs.paddingTop) || 0;
		const padB = parseFloat(cs.paddingBottom) || 0;
		const usableW = displayRef.clientWidth - padL - padR;
		const usableH = displayRef.clientHeight - padT - padB;
		const cols = Math.floor(usableW / cellW);
		const rows = Math.floor(usableH / cellH);
		return cols > 0 && rows > 0 ? { cols, rows } : null;
	}
	function shouldPublishActive(): boolean {
		return !unmounted
			&& !pending()
			&& isPageVisible()
			&& props.inLayout === true
			&& props.surfaceVisible
			&& props.surfaceActive;
	}
	function publishInactive(): void {
		clearFrameActivity();
		clearCursorBlink();
		setViewportLiveReady(false);
		backfillRef?.suspend();
		if (viewportTimer) {
			clearTimeout(viewportTimer);
			viewportTimer = null;
		}
		viewportCandidate = null;
		if (unmeasuredRaf !== 0) {
			cancelAnimationFrame(unmeasuredRaf);
			unmeasuredRaf = 0;
		}
		unmeasuredRetryUsed = false;
		view?.setInactive();
	}
	function parkView(): void {
		releasePaintHolds();
		publishInactive();
	}
	function retryUnmeasuredViewport(): void {
		if (unmeasuredRaf !== 0 || unmeasuredRetryUsed) return;
		unmeasuredRetryUsed = true;
		unmeasuredRaf = requestAnimationFrame(() => {
			unmeasuredRaf = 0;
			if (shouldPublishActive()) publishViewport();
		});
	}
	function publishViewport(): boolean {
		if (!displayRef || !view) return false;
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
			sid: sessionId,
			view_id: view.viewId,
			cols: measured.cols,
			rows: measured.rows,
		});
		return true;
	}
	function scheduleViewport(): void {
		clearTimeout(viewportTimer ?? undefined);
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
	}
	function publishViewportNow(): boolean {
		if (viewportTimer) {
			clearTimeout(viewportTimer);
			viewportTimer = null;
		}
		viewportCandidate = null;
		return publishViewport();
	}
	onMount(() => {
		view = createTerminalView(sessionId);
		const releaseViewStatus = view.subscribeStatus((status) => {
			setViewStatus(status);
			setViewportLiveReady(
				status.status === "accepted"
					&& status.active
					&& status.baselineReady,
			);
		});
		const releaseViewProgress = view.subscribeProgress(setAttachProgress);
		try {
		// Sync v2 owns connection and replay; no per-pane input channel startup.
		const sid = sessionId;
		const cellOwner = getOwner();
		runWithOwner(cellOwner, () => {
			// ── output: cells ────────────────────────────────────────────────
			renderer = new CellGridRenderer(
				displayRef!,
				() => setHasReconciledFrame(true),
				refreshTerminalPresentation,
			);
			refreshCursorBlink();
		// Retained history is paged only after explicit scroll/find demand; a
		// literal-bottom full frame paints only the current viewport.
		const backfill = createScrollbackBackfill({
			sessionId: props.session.id,
			renderer: () => renderer,
			active: () => props.inLayout === true
				&& props.surfaceVisible
				&& props.surfaceActive
				&& isPageVisible(),
		});
		backfillRef = backfill;
		unregisterUserInput = registerUserTerminalInput(sid, prepareLiveInteraction);
		// Only an interactive surface can produce genuine scrollbar/accessibility
		// intent. Hidden or parked scrolls are lifecycle cleanup and must restore
		// the newest canonical frame before the surface can be revealed.
		const onScroll = () => {
			if (!renderer || !displayRef) return;
			const interactive = props.inLayout === true
				&& props.surfaceVisible
				&& props.surfaceActive
				&& isPageVisible();
			if (!interactive) {
				notifyBackfill(renderer.prepareLiveInteraction());
				return;
			}
			notifyBackfill(renderer.handleScroll());
			if (!renderer.atBottom() && renderer.nearHistoryTop()) backfill.onUserScrollUp();
		};
		displayRef!.addEventListener("scroll", onScroll, { passive: true });
		// Ghost cursors: this viewer's latest cursor (sent to others) + the map of
		// other viewers' cursors received through presence metadata.
		let lastCurRow = 0;
		let lastCurCol = 0;
		const unregPreview = registerRenderer(props.session.id, renderer, () => {
			let cssVisible: boolean | null = null;
			if (displayRef?.isConnected) {
				const rect = displayRef.getBoundingClientRect();
				cssVisible = rect.width > 0 && rect.height > 0;
				for (let node: HTMLElement | null = displayRef; cssVisible && node; node = node.parentElement) {
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
				handler_canonical: renderer?.canonicalEpochSeq()
					?? { grid_epoch: null, seq: null },
				slot: {
					connected: displayRef?.isConnected === true,
					in_layout: props.inLayout ?? null,
					surface_active: props.surfaceActive,
					css_visible: cssVisible,
				},
				visibility: {
					document_visible: document.visibilityState === "visible",
					page_visible: isPageVisible(),
				},
			};
		});
		const ghostMap = new Map<
			string,
			{ x: number; y: number; label?: string }
		>();
		let unsubscribeRenderer: () => void;
		let unsubPresence: () => void;
		let measurementRaf = 0;
		runWithOwner(cellOwner, () => {
			unsubscribeRenderer = view!.subscribeRenderer(renderer!, ({ frame }) => {
				const diagOn = isDiagEnabled();
				const frameArrivedAt = diagOn ? performance.now() : 0;
				const sendTs = consumeLastInputSendTs(props.session.id);
				if (sendTs !== undefined) {
					const rttMs = performance.now() - sendTs;
					recordInputRtt(rttMs);
					if (rttMs > 0 && rttMs < 5000) {
						diag("echo.frame_rtt", {
							sid: props.session.id,
							rtt_ms: rttMs,
						});
					}
				}
				if (diagOn) {
					diag("cell.apply", {
						sid: props.session.id,
						stream_id: frame.streamId,
						seq: frame.seq,
						full: frame.full,
						vp_rows: frame.viewportRows.length,
						cursor_vis: frame.cursorVisible,
						cursor_row: frame.cursorRow,
						cursor_col: frame.cursorCol,
					});
				}
				setAltScreen(frame.altScreen);
				if (revealT0 !== 0) {
					diag("cell.reveal", {
						sid: props.session.id,
						ms: Math.round(performance.now() - revealT0),
						full: frame.full,
					});
					revealT0 = 0;
				}
				if (
					diagOn
					&& isPageVisible()
					&& props.inLayout === true
					&& props.surfaceActive
				) {
					requestAnimationFrame(() => requestAnimationFrame(() => {
						const canonical = renderer?.canonicalEpochSeq()
							?? { grid_epoch: null, seq: null };
						const reconciled = renderer?.reconciledEpochSeq()
							?? { grid_epoch: null, seq: null };
						diag("cell.dom_reconcile_opportunity", {
							sid: props.session.id,
							dur_ms: performance.now() - frameArrivedAt,
							canonical_epoch: canonical.grid_epoch,
							canonical_seq: canonical.seq,
							reconciled_epoch: reconciled.grid_epoch,
							reconciled_seq: reconciled.seq,
							block_reason: renderer?.reconcileBlockReason() ?? null,
						});
					}));
				}
				frameCursorApp = frame.cursorKeysApp;
				noteFrameActivity(frame);
				frameBracketed = frame.bracketedPaste;
				frameMouseSgr = frame.mouseSgr;
				frameFocusEvents = frame.focusEvents;
				setMouseTracking(frame.mouseTracking);
				lastCurRow = frame.cursorRow;
				lastCurCol = frame.cursorCol;
				predictor?.onFrame(frame);
				if (frame.full) backfill.onFullFrame();
			});
			markPhase("terminal_mount", { sessionId: props.session.id });
			// Spawn geometry is only an initial PTY-size hint. The real mounted
			// view always attaches normally after the opened event.
			const publishSpawnMeasurement = (): boolean => {
				if (
					!pending()
					|| props.inLayout !== true
					|| !props.surfaceVisible
					|| !props.surfaceActive
					|| !isPageVisible()
				) return false;
				const measured = measureViewport();
				if (!measured) return false;
				return publishMountedSpawnMeasurement(props.session.id, measured);
			};
			if (!publishSpawnMeasurement()) {
				measurementRaf = requestAnimationFrame(() => {
					measurementRaf = 0;
					publishSpawnMeasurement();
				});
			}
			if (shouldPublishActive()) publishViewport();
			predictor = new PredictiveEcho(renderer!.predictionHost, {
				mode: predictMode,
				sid: props.session.id,
				onCursor: (col) => renderer?.setPredictedCursor(col),
			});
			createEffect(() => {
				predictMode();
				predictor?.refreshPreference();
			});
			// Build-time gate first: prod bundles fold the debug hook away.
			if (import.meta.env.VITE_ROOST_SMOKE === "1"
				&& typeof localStorage !== "undefined" && localStorage.getItem("roostSmoke") === "1") {
				(window as Window & { __roostPredictDebug?: () => unknown }).__roostPredictDebug =
					() => predictor?._debug() ?? null;
			}

			inputController = new TerminalInputController(displayRef!, {
				cursorKeysApplication: () => frameCursorApp,
				focusEventsEnabled: () => frameFocusEvents,
				onData: (data) => {
					const armed = ctrlArmed();
					const controlledData = armed ? applyCtrlModifier(data) : data;
					if (armed) setCtrlArmed(false);
					const bytes = new TextEncoder().encode(controlledData);
					const admission = sendUserTerminalInput(
						props.session.id,
						bytes,
						view?.viewId,
					);
					if (!admission.accepted) {
						signal("input.drop_burst", {
							sid: props.session.id,
							reason: admission.reason,
							cooldownKey: props.session.id,
						});
						return;
					}
					predictor?.predict(bytes);
					recordInput(props.session.id, controlledData);
					void admission.result.then((result) => {
						if (result.status === "accepted") return;
						signal("input.drop_burst", {
							sid: props.session.id,
							reason: result.status === "ambiguous" ? "ambiguous" : result.reason,
							cooldownKey: props.session.id,
						});
					});
				},
				onPaste: (text, event) => {
					enqueueFileItems(event.clipboardData?.items);
					pasteText(text);
				},
				ariaLabel: `Terminal input — ${sessionTitle(props.session)}`,
			});
			if (
				props.inLayout === true
				&& props.focused === true
				&& !isTouchDevice()
				&& activeComposeSessionId() === null
			) {
				inputController.forceFocus();
				requestAnimationFrame(() => {
					if (
						!unmounted
						&& props.inLayout === true
						&& props.focused === true
						&& activeComposeSessionId() === null
					) inputController?.forceFocus();
				});
			}
			// Receive remote viewers' cursors → ghostMap → cellRenderer (ch/lh overlay).
			unsubPresence = registerPresenceHandler(props.session.id, (msg) => {
				const f = msg as {
					kind?: string;
					viewer_id?: string;
					cursor_col?: number;
					cursor_row?: number;
					label?: string;
				};
				if (f.kind === "presence-delta" && typeof f.viewer_id === "string") {
					ghostMap.set(f.viewer_id, {
						x: f.cursor_col ?? 0,
						y: f.cursor_row ?? 0,
						label: f.label ?? f.viewer_id,
					});
					renderer?.setGhosts(ghostMap);
				} else if (
					f.kind === "presence-leave"
					&& typeof f.viewer_id === "string"
				) {
					if (ghostMap.delete(f.viewer_id)) renderer?.setGhosts(ghostMap);
				}
			});
		});
		// Send THIS viewer's cursor so others' ghostMaps update. Gated on visible +
		// active (a hidden deck pane has no one watching). 500ms, only on change.
		let lastSentRow = -1,
			lastSentCol = -1;
		const releaseCursorPoll = registerCursorPoll(() => {
			if (props.inLayout !== true || !props.surfaceActive || !isPageVisible()) return;
			if (lastCurRow === lastSentRow && lastCurCol === lastSentCol) return;
			lastSentRow = lastCurRow;
			lastSentCol = lastCurCol;
			void coordClient.sessionsCursorPos({
				sessionId: props.session.id,
				col: lastCurCol,
				row: lastCurRow,
			});
		});
		// Linkify rendered .cell-row text: regex URLs + resolvable file paths,
		// Cmd/Ctrl-gated. OSC 8 producer links need no scan — the renderer paints
		// them straight from the core's per-cell link data (cellRow.ts).
		linkAttachment = attachTerminalLinks(displayRef!, {
			resolveFile,
			// Cmd/Ctrl-click a file path → download it from the worker (works whether
			// the session is local or on another Mac), replacing the in-app viewer.
			onOpenFile: (href) => void downloadWorkerFileByHref(href),
			githubOwnerRepo: () => props.session.git_remote ?? undefined,
			// Freeze cell repaints while Cmd-hovering a link so the wrapped <a> stops
			// churning under the cursor (the pointer↔text flicker).
			onArmedHoverChange: (active) => {
				notifyBackfill(renderer?.setArmedHold(active));
			},
		});

		// MOBILE keyboard fix: tapping a sidebar row to switch terminals closes the
		// overlay, and that tap's click falls through onto the revealed display →
		// forceFocus → keyboard pops, unwanted. Record when this pane becomes active
		// (transition-only via prevActive) and skip focus within NAV_FALLTHROUGH_MS
		// on touch. A fall-through tap started on the OVERLAY and synthesized mouse
		// events target the touchstart element, so the display sees a click with NO
		// mousedown: click applies the window only then, never to a real tap.
		const NAV_FALLTHROUGH_MS = 700;
		let lastActivatedMs = 0, prevActive = false, gestureStartedOnDisplay = false;
		createEffect(() => {
			const active = props.focused === true;
			if (active && !prevActive) lastActivatedMs = Date.now();
			prevActive = active;
		});
		const isNavFallthrough = () =>
			isTouchDevice() && Date.now() - lastActivatedMs < NAV_FALLTHROUGH_MS;

		// Clicks land on visible cells, so bridge them back to this pane's
		// textarea after native selection/focus handling settles.
		const onDisplayDown = (ev: MouseEvent) => {
			// A direct pointer interaction supersedes any unmatched browser cleanup
			// bracket before a scrollbar drag or a new selection can scroll.
			renderer?.finishLiveSelectionRelease();
			// Left-click only: right-click (button 2) opens the context menu and
			// must NOT steal focus — forceFocus collapses the window selection, and
			// its microtask runs before `contextmenu` reads getSelection(), so the
			// "Copy" item would never see the marked text.
			if (ev.button !== 0) return;
			gestureStartedOnDisplay = true; // a fall-through click has no mousedown here
			if (isNavFallthrough()) return;
			const t = ev.target as HTMLElement | null;
			if (t?.closest("button, input, textarea, a")) return;
			queueMicrotask(() => inputController?.forceFocus());
		};
		displayRef!.addEventListener("mousedown", onDisplayDown);

		// A trusted click can move focus to body after mousedown. Restore terminal
		// focus after the click settles unless the user has selected text.
		const onDisplayClick = (ev: MouseEvent) => {
			const startedHere = gestureStartedOnDisplay; gestureStartedOnDisplay = false;
			if (ev.button !== 0 || (!startedHere && isNavFallthrough())) return; // fall-through, not a real tap
			const t = ev.target as HTMLElement | null;
			if (t?.closest("button, input, textarea, a")) return;
			const sel = displayRef?.ownerDocument.getSelection();
			if (sel && !sel.isCollapsed) return; // text selected → don't steal focus
			inputController?.forceFocus();
		};
		displayRef!.addEventListener("click", onDisplayClick);

		// Renderer DOM replacement would otherwise destroy an active pane
		// selection. The completed noncollapsed selection is explicit reading;
		// cross-boundary selections count when either endpoint belongs here.
		const onSelectionChange = () => {
			syncNativeSelectionHold();
		};

		// Copy-on-select (opt-in): the tmux/xterm habit of putting the selection on
		// the clipboard when the gesture ENDS. Deliberately does NOT clear the
		// selection — that would release setSelectionHold above and let the renderer
		// repaint under the user's mouse mid-drag.
		const onSelectionSettled = () => {
			if (!copyOnSelect()) return;
			const sel = displayRef?.ownerDocument.getSelection();
			if (!sel || sel.isCollapsed) return;
			if (!sel.anchorNode || !displayRef!.contains(sel.anchorNode)) return;
			void copySelectionToClipboard();
		};

		// ── mouse / touch forwarding ─────────────────────────────────────────
		// Pane-local press/touch listeners attach here; the drag continuation and
		// the wheel/touchmove passivity swap are wired below, each on its own
		// lifetime. See lib/terminalMouseForwarding.ts.
		const mouseForwarding = attachTerminalMouseForwarding({
			display: displayRef!,
			mouseTracking,
			sendBytes: (bytes) =>
				sendUserTerminalInput(props.session.id, bytes, view?.viewId),
			getRenderer: () => renderer,
			getMouseSgr: () => frameMouseSgr,
			getCellW: () => cellW,
			getCellH: () => cellH,
			measureCell,
		});

		// att1d — file drop/paste → upload → inject abs_path into the PTY.
		// Lost in the byte-mode cut (e8f450b9); restored here. Logic lives in
		// attachments.ts::enqueueAttachment (same path as the touch picker).
		//
		// Listen on DOCUMENT (not just the cell grid): in cell mode the grid is
		// letterboxed (narrower than the pane), so a drop on the surrounding margin
		// missed the grid handler → the browser opened the image in a new tab.
		// file drags only so text/selection drags aren't hijacked.
		const dragHasFiles = (e: DragEvent) => e.dataTransfer?.types.includes("Files") ?? false;
		const onDragOver = (e: DragEvent) => {
			if (!props.focused || !isPageVisible() || !dragHasFiles(e)) return;
			e.preventDefault(); // allow the drop + stop the browser opening the file
		};
		const onDrop = (e: DragEvent) => {
			if (!props.focused || !isPageVisible() || !dragHasFiles(e)) return;
			e.preventDefault();
			enqueueFileItems(e.dataTransfer?.items);
		};

		createEffect(on(viewActiveFlag, (active) => {
			refreshCursorBlink();
			if (!active) {
				parkView();
				return;
			}
			revealT0 = performance.now();
			publishViewportNow();
		}));
		// Focus gate: only the focused pane's terminal grabs the keyboard. Touch
		// devices skip it (an explicit tap on the display still focuses) so selecting
		// a pane doesn't pop the on-screen keyboard. This effect is intentionally
		// non-deferred: selection must focus in the same reactive turn.
		const focusGate = createMemo(
			() => viewActiveFlag() && props.focused === true,
		);
		createEffect(() => {
			if (!focusGate()) {
				setCtrlArmed(false);
				return;
			}
			if (!isTouchDevice() && activeComposeSessionId() === null) inputController?.forceFocus();
		});


		// Per-pane GLOBAL listeners attach only while this pane is in the layout;
		// parked tabs stay mounted without multiplying document event work.
		runWithOwner(cellOwner, () =>
			createEffect(() => {
				if (!viewActiveFlag()) return;
				document.addEventListener("selectionchange", onSelectionChange);
				window.addEventListener("pointerup", onSelectionSettled);
				window.addEventListener("keyup", onSelectionSettled);
				window.addEventListener("mousemove", mouseForwarding.onWindowMouseMove);
				window.addEventListener("mouseup", mouseForwarding.onWindowMouseUp);
				document.addEventListener("dragenter", onDragOver);
				document.addEventListener("dragover", onDragOver);
				document.addEventListener("drop", onDrop);
				onCleanup(() => {
					document.removeEventListener("selectionchange", onSelectionChange);
					window.removeEventListener("pointerup", onSelectionSettled);
					window.removeEventListener("keyup", onSelectionSettled);
					window.removeEventListener("mousemove", mouseForwarding.onWindowMouseMove);
					window.removeEventListener("mouseup", mouseForwarding.onWindowMouseUp);
					document.removeEventListener("dragenter", onDragOver);
					document.removeEventListener("dragover", onDragOver);
					document.removeEventListener("drop", onDrop);
				});
			}),
		);

		// Wheel/touchmove passivity swaps on every tracking-mode flip. Why
		// remove-then-add matters lives with the listeners in
		// lib/terminalMouseForwarding.ts.
		runWithOwner(cellOwner, () => mouseForwarding.bindWheelAndTouchMove());

		// Name the grid's log region for assistive tech, and keep it current as the
		// session is renamed or its running program changes.
		runWithOwner(cellOwner, () =>
			createEffect(() => {
				const title = sessionTitle(props.session);
				renderer?.setAccessibleLabel(`Terminal — ${title}`);
				inputController?.setAccessibleLabel(`Terminal input — ${title}`);
			}),
		);

		// When an optimistic spawn resolves, attach through the ordinary view
		// command. Spawn dimensions were only the PTY's initial-size hint.
		createEffect(() => {
			if (pending() || !viewActiveFlag()) return;
			scheduleViewport();
		});

		createEffect(() => {
			const px = termFontSize();
			if (px === lastZoomPx) return;
			lastZoomPx = px;
			cellW = 0;
			cellH = 0;
			renderer?.invalidateRowHeight();
			scheduleViewport();
		});
		resizeObs = new ResizeObserver(() => {
			notifyBackfill(renderer?.noteBoxResize());
			if (isResizeDragging()) return;
			scheduleViewport();
		});
		resizeObs.observe(displayRef!);

		// Divider/sidebar drag publishes one settled size on release.
		let wasResizeDragging = false;
		createEffect(() => {
			const dragging = isResizeDragging();
			if (dragging) {
				if (viewportTimer) {
					clearTimeout(viewportTimer);
					viewportTimer = null;
				}
			} else if (wasResizeDragging && viewActiveFlag() && isPageVisible()) {
				publishViewport();
			}
			wasResizeDragging = dragging;
		});

		const spotlitFlag = createMemo(() => !!props.spotlit);
		let wasSpotlit = false;
		createEffect(() => {
			const spotlit = spotlitFlag();
			if (spotlit === wasSpotlit) return;
			wasSpotlit = spotlit;
			requestAnimationFrame(() => {
				if (viewActiveFlag() && isPageVisible()) scheduleViewport();
			});
		});

		createEffect(on(arrangeEpoch, () => {
			requestAnimationFrame(() => {
				if (viewActiveFlag() && isPageVisible()) scheduleViewport();
			});
		}, { defer: true }));

		const onVisibility = () => {
			if (!viewActiveFlag() || !isPageVisible()) {
				parkView();
				return;
			}
			refreshCursorBlink();
			refreshTerminalPresentation();
			publishViewportNow();
		};
		document.addEventListener("visibilitychange", onVisibility);
		const onWindowResize = () => {
			if (viewActiveFlag() && isPageVisible()) scheduleViewport();
		};
		window.addEventListener("resize", onWindowResize);

		const onPageHide = () => {
			clearFrameActivity();
			clearCursorBlink();
			releasePaintHolds();
			publishInactive();
		};
		const onPageShow = () => {
			refreshCursorBlink();
			refreshTerminalPresentation();
			if (isPageVisible() && viewActiveFlag()) publishViewportNow();
		};
		window.addEventListener("pagehide", onPageHide);
		window.addEventListener("pageshow", onPageShow);

		// The pane textarea lives offscreen; clicking bare chrome can park focus on
		// a control. Capture-phase prevention plus key recovery keeps ownership
		// stable without interfering with real inputs, dialogs, menus or panes.
		//
		// (1) mousedown PREVENT — a click on anything outside FOCUS_OWNERS keeps
		//     focus on the terminal (preventDefault blocks the focus-shift; the
		//     click still fires, so the button works). This is the primary fix —
		//     focus never leaves, so no keystroke is lost.
		const onDocMousedown = (e: MouseEvent) => {
			if (!props.focused || !props.surfaceActive || !isPageVisible()) return;
			if (e.button !== 0) return; // left only; right-click → context menu
			const t = e.target as HTMLElement | null;
			// Allow clicks inside ANY pane (this one or another) — clicking a pane must
			// be able to focus it. Only bare chrome OUTSIDE every pane keeps focus here.
			if (t?.closest(FOCUS_OWNERS) || t?.closest("[data-pane]")) return;
			e.preventDefault();
		};
		document.addEventListener("mousedown", onDocMousedown, true);
		//
		// Keydown recovery is the backstop for an overlay that grabbed focus and
		// later dropped it. The textarea identity check keeps the normal
		// per-keystroke path off every selector walk.
		const onDocKeydown = (e: KeyboardEvent) => {
			if (e.defaultPrevented) return;
			if (!e.isTrusted && isTouchDevice()) return;

			if (!props.focused || !isPageVisible()) return;
			// Copy / paste chords. Placed before the focus-recovery machinery below
			// so they work regardless of where focus currently sits, and
			// preventDefault'd so the byte never reaches the PTY as ^C / ^V.
			if ((e.metaKey || e.ctrlKey) && e.shiftKey && !e.altKey) {
				const k = e.key.toLowerCase();
				if (k === "c") {
					e.preventDefault();
					e.stopPropagation();
					void copySelectionToClipboard();
					return;
				}
				if (k === "v") {
					e.preventDefault();
					e.stopPropagation();
					void pasteFromClipboard();
					return;
				}
			}
			// Find opens on ⌘F (macOS) or Ctrl+⇧F (the gnome-terminal shape). NEVER
			// plain Ctrl+F: that is readline's forward-char and every TUI's own
			// binding, and this listener is capture-phase — taking it here would
			// steal the byte before wterm could encode it. The sidebar filter yields
			// ⌘F while a deck is on screen (AllView.tsx), so one owner per chord.
			if (
				!e.altKey && e.key.toLowerCase() === "f" &&
				((e.metaKey && !e.ctrlKey && !e.shiftKey) || (e.ctrlKey && e.shiftKey))
			) {
				e.preventDefault();
				e.stopPropagation();
				find.openFind();
				return;
			}
			const ae = document.activeElement as HTMLElement | null;
			if (inputController?.ownsTarget(ae)) return;
			if (ae === document.body || ae === document.documentElement) {
				const altGraph = isAltGraphKey(e);
				if (e.metaKey || (e.altKey && !altGraph) || e.isComposing) return;
				if (e.key === "Control" || e.key === "Shift") return;

				inputController?.forceFocus();
				if (inputController?.dispatchKeydown(e.key, {
					code: e.code,
					ctrlKey: e.ctrlKey,
					shiftKey: e.shiftKey,
					altKey: e.altKey,
					metaKey: e.metaKey,
					altGraph,
				})) {
					e.preventDefault();
					e.stopPropagation();
				}
				return;
			}
			if (e.metaKey || e.ctrlKey || e.altKey) return;
			if (ae && ae.closest(FOCUS_OWNERS)) return;
			if (e.key.length !== 1 || e.key === " ") return;
			diag("focus.recover", {
				sid: props.session.id,
				via: "keydown",
				key: "char",
			});
			inputController?.forceFocus();
		};
		document.addEventListener("keydown", onDocKeydown, true);

		runWithOwner(cellOwner, () =>
			onCleanup(() => {
				unregisterUserInput?.();
				unregisterUserInput = null;
				releasePaintHolds();
				unsubscribeRenderer();
				unsubPresence();
				linkAttachment?.dispose();
				linkAttachment = null;
				releaseCursorPoll();
				document.removeEventListener("visibilitychange", onVisibility);
				window.removeEventListener("resize", onWindowResize);
				window.removeEventListener("pagehide", onPageHide);
				window.removeEventListener("pageshow", onPageShow);
				document.removeEventListener("keydown", onDocKeydown, true);
				document.removeEventListener("mousedown", onDocMousedown, true);
				publishInactive();
				releaseViewStatus();
				releaseViewProgress();
				view?.dispose();
				backfill.dispose();
				backfillRef = null;
				displayRef?.removeEventListener("scroll", onScroll);
				displayRef?.removeEventListener("mousedown", onDisplayDown);
				displayRef?.removeEventListener("click", onDisplayClick);
				mouseForwarding.dispose();
				resizeObs?.disconnect();
				clearTimeout(viewportTimer ?? undefined);
				if (measurementRaf !== 0) cancelAnimationFrame(measurementRaf);
				if (unmeasuredRaf !== 0) cancelAnimationFrame(unmeasuredRaf);

				predictor?.dispose();
				unregPreview();
				renderer?.dispose();
				inputController?.destroy();
				clearInput(sid);
				predictor = null;
				renderer = null;
				inputController = null;
				view = null;
			}),
		);
		});
		} catch (e) {
			unregisterUserInput?.();
			unregisterUserInput = null;
			inputController?.destroy();
			inputController = null;
			publishInactive();
			releaseViewStatus();
			releaseViewProgress();
			view?.dispose();
			view = null;
			// A synchronous pane setup failure must surface instead of leaving a
			// silent, painted-but-untypable terminal.
			signal("diag.corruption_signal", {
				kind: "cell_mount_failed",
				sid: props.session.id,
				session_trace_id: getSessionTraceId(props.session.id),
				msg: String(e),
				cooldownKey: props.session.id,
			});
		}
	});

	onCleanup(() => {
		unmounted = true;
		clearFrameActivity();
		clearCursorBlink();
	});

	// Deepgram keyterm biasing input for the composer's inline mic.
	const readTerminalContext = (): TerminalContext => ({
		grid: renderer?.gridText() ?? "",
		scrollback: renderer?.scrollbackText() ?? "",
		input: getInputText(props.session.id),
	});

	return (
		<div
			data-testid="cell-terminal-pane"
			data-session-id={props.session.id}
			style={{
				position: "absolute",
				inset: "0",
				display: "flex",
				"flex-direction": "column",
				"min-height": "0",
			}}
		>
			<Show when={terminalPresentationState() === "receiving"}>
				<div
					class="terminal-stream-indicator"
					data-testid="terminal-stream-indicator"
					data-state="receiving"
					title="Receiving terminal frames"
					aria-hidden="true"
				/>
			</Show>
			<Show when={terminalPresentationState() === "catching_up"}>
				<div
					class="terminal-stream-indicator"
					data-testid="terminal-stream-indicator"
					data-state="catching_up"
					title="Screen catching up"
					aria-hidden="true"
				/>
			</Show>
			{/* Above the display and inside the pane: the bar consumes real rows, so
          ResizeObserver publishes the smaller viewport. */}
			<Show when={find.open()}>
				<TerminalFindBar
					find={find}
					altScreen={altScreen()}
					onDismiss={() => { find.closeFind(); inputController?.forceFocus(); }}
				/>
			</Show>
			<div
				ref={displayRef}
				data-testid="terminal-display"
				style={{ flex: "1", "min-width": "0", "min-height": "0", "touch-action": "pan-y" }}
			/>
			{/* Unbracketed multi-line paste confirmation. Registered in FOCUS_OWNERS
          (md-dialog) so the pane's focus guards let its buttons take focus.
          Mounted ONLY while a paste is pending: md-dialog keeps its slotted
          content in the DOM even when closed, and this subtree sits inside the
          pane, so leaving it mounted would fold dialog prose into the pane's
          textContent — which the smoke harness reads as terminal output. */}
			<Show when={pendingPaste() !== null}>
				<Dialog
					open
					onClose={() => setPendingPaste(null)}
					headline="Paste multiple lines?"
					actions={
						<>
							<Button variant="text" data-testid="paste-guard-cancel" onClick={() => setPendingPaste(null)}>
								Cancel
							</Button>
							<Button
								variant="filled"
								data-testid="paste-guard-send"
								onClick={() => {
									const text = pendingPaste();
									setPendingPaste(null);
									if (text !== null) sendTerminalText(text);
								}}
							>
								Paste {pendingPasteLines()} lines
							</Button>
						</>
					}
				>
					<p class="md-body-m" style={{ margin: "0" }}>
						This shell has bracketed paste off, so all {pendingPasteLines()} lines run
						as they arrive — you will not get a chance to edit them first.
					</p>
				</Dialog>
			</Show>
			{/* Compact has one active, body-portaled composer and keypad. Keep both
			    unmounted while the mobile drawer or a non-terminal overlay is open. */}
			<Show when={
				props.inLayout === true
				&& props.focused === true
				&& isCompact()
				&& !uiStore.sidebarOpen
				&& props.surfaceVisible
			}>
				<TerminalNavButtons
					onKey={(key: string) => { inputController?.dispatchKeydown(key); }}
					ctrlArmed={ctrlArmed()}
					onCtrlArmedChange={(armed: boolean) => {
						if (armed && !isTouchDevice()) inputController?.forceFocus();
						setCtrlArmed(armed);
					}}
				/>
				<TerminalComposeButton
					placement="viewport"
					session={props.session}
					active={props.surfaceActive}
					onSubmit={(text) => sendTerminalText(text, true)}
					onAttachFiles={attachSelectedFiles}
					readContext={readTerminalContext}
					captureTerminalSelection={captureTerminalSelection}
				/>
			</Show>
			{/* Keep every mounted desktop session's inline composer alive while its
			    slot is parked. That preserves the exact display height across tab
			    reveals; only each pane's selected slot is visible. */}
			<Show when={!isCompact()}>
				<TerminalComposeButton
					placement="pane"
					session={props.session}
					active={props.inLayout === true && props.surfaceActive}
					onSubmit={(text) => sendTerminalText(text, true)}
					onAttachFiles={attachSelectedFiles}
					readContext={readTerminalContext}
					captureTerminalSelection={captureTerminalSelection}
				/>
			</Show>
			<TerminalContextMenu
				session={props.session}
				getContainer={() => displayRef ?? null}
				onAttachFile={attachSelectedFiles}
				onPasteText={pasteText}

			/>
			<Show when={loadingNotice()}>
				{(notice) => (
					<TerminalLoadingNotice
						{...notice()}
						progress={loadingProgress()}
						stuckReason={stuckReason()}
					/>
				)}
			</Show>
			<Show when={offline()}>
				<TerminalOfflineNotice
					onRetry={retryOffline}
					onOpenSibling={openOfflineSibling}
					hasSibling={!!offlineSibling()}
				/>
			</Show>
		</div>
	);
}
