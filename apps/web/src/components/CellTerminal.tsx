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
import {
	CellGridRenderer,
	type LiveInteractionResult,
} from "../lib/cellRenderer.ts";
import { createScrollbackBackfill } from "../lib/scrollbackBackfill.ts";
import { PredictiveEcho } from "../lib/predictiveEcho.ts";
import { TerminalContextMenu } from "./TerminalContextMenu.tsx";
import { pickAndAttachFiles, enqueueAttachment } from "../lib/attachments.ts";
import type { TerminalContext } from "../lib/keytermContext.ts";
import {
	TerminalComposeButton,
	activeComposeSessionId,
	type TerminalSelectionGuard,
} from "./TerminalComposeButton.tsx";
import { TerminalNavButtons } from "./TerminalNavButtons.tsx";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { mouseForwardEnabled } from "../lib/mouseForwardPref.ts";
import {
	cellFromPoint,
	terminalMouseReport,
	type MouseGesture,
} from "../lib/terminalMouse.ts";
import type { MouseTracking } from "@roost/shared/cell";
import { isCompact, isTouchDevice } from "../lib/windowSizeClass.ts";
import { uiStore } from "../store/uiStore.ts";
import { FOCUS_OWNERS } from "../lib/focusOwners.ts";
import { attachTerminalLinks, type ResolveFile, type TerminalLinkAttachment } from "./terminal-links.ts";
import { downloadWorkerFileByHref } from "../lib/downloadWorkerFile.ts";
import { resolveWorkerPath, workerFileHref } from "../lib/nativePath.ts";
import { registerRenderer } from "../lib/terminalPreview.ts";
import {
	registerCellHandler,
	registerPresenceHandler,
	acquireCellMountClaim,
	type CellMountClaim,
} from "../store/sync.ts";
import {
	acquireTerminalViewportOwner,
	consumeLastInputSendTs,
	seedTerminalViewportIntent,
	type InputAdmission,
	type TerminalViewportOwner,
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
import { buildPtyPayload, CR_BYTES } from "../lib/ptyPaste.ts";
import { applyCtrlModifier, isAltGraphKey } from "../lib/terminalInput.ts";


import { coordClient } from "../connect.ts";
import { ResizeCause } from "@roost/shared/proto/coordinator_pb";
import { isResizeDragging, arrangeEpoch } from "../lib/resizeDrag.ts";
import { diag, isDiagEnabled, signal } from "@roost/shared/diag";
import { getSessionTraceId, markPhase, markPhaseOnce } from "../lib/diag.ts";
import { recordInputRtt } from "../lib/leakWatch.ts";
import { termFontSize } from "../lib/terminalFontPref.ts";
import { copyOnSelect } from "../lib/copyOnSelectPref.ts";
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
import { TerminalOfflineNotice } from "./TerminalOfflineNotice.tsx";
import {
	isPendingSpawn,
	publishMountedSpawnMeasurement,
	takeSpawnViewportSeed,
} from "../store/optimisticSpawn.ts";
import { predictMode } from "../lib/predictPref.ts";

interface CellTerminalProps {
	session: Session;
	// In the current tiling layout (a visible pane's selected tab) → claim size +
	// render cells. Not in layout → withdraw + go dormant (stays MOUNTED for the
	// persistent-deck no-remount guarantee). Replaces the old single `isActive`.
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
interface CapturedPaneSelection {
	epoch: number;
	display: HTMLDivElement;
	doc: Document;
	range: Range;
	selectedText: string;
	anchorNode: Node;
	anchorOffset: number;
	focusNode: Node;
	focusOffset: number;
	ownedRows: Array<{ row: HTMLElement; text: string }>;
}


const CLAIM_DEBOUNCE_MS = 150;
// Liveness heartbeat for the viewport claim. The worker reaps a claim after
// VIEWER_CLAIM_TTL_MS (120s, viewport.ts) of silence and then STOPS emitting
// cells to that viewer (_hasActiveViewer gate, session-manager.ts). A
// foreground tab left idle — no resize, no visibility flip; e.g. the machine
// slept — sends no claim, so after 120s the pane goes dead-on-type: input
// reaches the worker and updates the grid, but no cells come back until a
// refresh/tab-switch re-claims. 30s = ¼ TTL, matching the "viewers refresh
// every 30s" the worker's reaper comment already assumes.
const CLAIM_HEARTBEAT_MS = 30_000;





// Shared 500ms cursor-poll ticker — one interval for ALL mounted panes (was one
// per open session; the deck keeps every open session mounted). The ticker
// starts on first register and stops when the last pane unregisters.
// Per-instance gating (inLayout/visible/changed) stays in each callback.
const CURSOR_POLL_MS = 500;
const _cursorPollCbs = new Set<() => void>();
let _cursorPollHandle: number | null = null;
function _registerCursorPoll(cb: () => void): () => void {
	_cursorPollCbs.add(cb);
	if (_cursorPollHandle === null) {
		_cursorPollHandle = window.setInterval(() => {
			for (const f of _cursorPollCbs) f();
		}, CURSOR_POLL_MS);
	}
	return () => {
		_cursorPollCbs.delete(cb);
		if (_cursorPollCbs.size === 0 && _cursorPollHandle !== null) {
			clearInterval(_cursorPollHandle);
			_cursorPollHandle = null;
		}
	};
}

// Grace before a viewed-but-frameless pane is declared "not responding" (a dead
// breadcrumb: open session, no live PTY). Applied PER ATTEMPT: on expiry the
// watch silently re-claims (up to 2×, one grace apart) before showing the
// notice, so a truly dead breadcrumb surfaces only after ~OFFLINE_GRACE_MS × 3
// (~9s) while a live-but-transiently-stuck pane self-heals without ever showing
// it. Long enough that a slow first snapshot never false-flags a live pane; the
// notice self-clears the instant a frame lands. See lib/offlineWatch.ts.
const OFFLINE_GRACE_MS = 3000;



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
	let selectionGuardEpoch = 0;
	let activeSelectionGuard: TerminalSelectionGuard | null = null;
	let selectionGuardSuspended = false;
	const paneOwnsSelectionEndpoint = (selection: Selection): boolean =>
		(!!selection.anchorNode && !!displayRef?.contains(selection.anchorNode))
		|| (!!selection.focusNode && !!displayRef?.contains(selection.focusNode));
	const notifyBackfill = (result: LiveInteractionResult | undefined): void => {
		if (result?.anchorChanged) backfillRef?.onFullFrame();
	};
	const syncNativeSelectionHold = (): void => {
		const selection = displayRef?.ownerDocument.getSelection();
		const held =
			selectionGuardSuspended
			|| (
				!!selection
				&& !selection.isCollapsed
				&& selection.rangeCount > 0
				&& paneOwnsSelectionEndpoint(selection)
			);
		if (held) renderer?.enterReading("selection");
		notifyBackfill(renderer?.setSelectionHold(held));
	};
	const discardActiveSelectionGuardForTransition = (): void => {
		const guard = activeSelectionGuard;
		activeSelectionGuard = null;
		selectionGuardSuspended = false;
		guard?.release();
	};
	const captureTerminalSelection = (): TerminalSelectionGuard | undefined => {
		const display = displayRef;
		const doc = display?.ownerDocument;
		const selection = doc?.getSelection();
		if (
			!display
			|| !doc
			|| !selection
			|| selection.isCollapsed
			|| selection.rangeCount === 0
			|| !paneOwnsSelectionEndpoint(selection)
		) return;

		const anchorNode = selection.anchorNode;
		const focusNode = selection.focusNode;
		if (!anchorNode || !focusNode) return;
		const range = selection.getRangeAt(0).cloneRange();
		const selectedText = selection.toString();
		if (!selectedText) return;

		// Row identity is the safety boundary. A canonical repair may retain the
		// same text while replacing its nodes; never resurrect that detached range.
		const anchorElement = anchorNode instanceof Element ? anchorNode : anchorNode.parentElement;
		const focusElement = focusNode instanceof Element ? focusNode : focusNode.parentElement;
		const anchorCandidate = anchorElement?.closest(".cell-row");
		const focusCandidate = focusElement?.closest(".cell-row");
		const anchorRow = anchorCandidate instanceof HTMLElement ? anchorCandidate : null;
		const focusRow = focusCandidate instanceof HTMLElement ? focusCandidate : null;
		const ownedRows: Array<{ row: HTMLElement; text: string }> = [];
		if (anchorRow && display.contains(anchorRow)) {
			ownedRows.push({ row: anchorRow, text: anchorRow.textContent ?? "" });
		}
		if (focusRow && focusRow !== anchorRow && display.contains(focusRow)) {
			ownedRows.push({ row: focusRow, text: focusRow.textContent ?? "" });
		}
		if (ownedRows.length === 0) return;

		let captured: CapturedPaneSelection | null = {
			epoch: selectionGuardEpoch,
			display,
			doc,
			range,
			selectedText,
			anchorNode,
			anchorOffset: selection.anchorOffset,
			focusNode,
			focusOffset: selection.focusOffset,
			ownedRows,
		};
		const validCapture = (): CapturedPaneSelection | null => {
			const saved = captured;
			if (!saved) return null;
			const anchorValue = saved.anchorNode.nodeValue;
			const focusValue = saved.focusNode.nodeValue;
			const anchorLength = anchorValue === null
				? saved.anchorNode.childNodes.length
				: anchorValue.length;
			const focusLength = focusValue === null
				? saved.focusNode.childNodes.length
				: focusValue.length;
			if (
				saved.epoch !== selectionGuardEpoch
				|| displayRef !== saved.display
				|| !saved.display.isConnected
				|| !saved.anchorNode.isConnected
				|| !saved.focusNode.isConnected
				|| saved.anchorNode.getRootNode() !== saved.doc
				|| saved.focusNode.getRootNode() !== saved.doc
				|| (
					!saved.display.contains(saved.anchorNode)
					&& !saved.display.contains(saved.focusNode)
				)
				|| saved.anchorOffset > anchorLength
				|| saved.focusOffset > focusLength
				|| !saved.range.startContainer.isConnected
				|| !saved.range.endContainer.isConnected
				|| saved.range.toString() !== saved.selectedText
			) {
				captured = null;
				return null;
			}
			for (const owned of saved.ownedRows) {
				if (
					!owned.row.isConnected
					|| !saved.display.contains(owned.row)
					|| owned.row.textContent !== owned.text
				) {
					captured = null;
					return null;
				}
			}
			return saved;
		};
		const selectionMatchesCapture = (
			current: Selection,
			saved: CapturedPaneSelection,
		): boolean =>
			current.anchorNode === saved.anchorNode
			&& current.anchorOffset === saved.anchorOffset
			&& current.focusNode === saved.focusNode
			&& current.focusOffset === saved.focusOffset;
		const currentSelection = (saved: CapturedPaneSelection): Selection | null => {
			const current = saved.doc.getSelection();
			if (!current) {
				captured = null;
				return null;
			}
			// Never clear or overwrite a new native range established by another
			// owner. A collapsed range is the browser's editable-focus artifact.
			if (!current.isCollapsed && !selectionMatchesCapture(current, saved)) {
				captured = null;
				return null;
			}
			return current;
		};
		const guard: TerminalSelectionGuard = {
			suspend(): boolean {
				const saved = validCapture();
				if (!saved) return false;
				const current = currentSelection(saved);
				if (!current) return false;
				// A focused textarea cannot begin its native editing command while
				// this document range remains active. Yield only the exact retained
				// pane range while its explicit suspended state keeps the renderer's
				// selection hold live across asynchronous selectionchange delivery.
				const restored = selectionMatchesCapture(current, saved);
				if (restored && current.rangeCount !== 1) {
					captured = null;
					return false;
				}
				selectionGuardSuspended = true;
				if (restored) {
					// Chromium resets the native editing target only through the
					// Selection-wide clear; rangeCount === 1 makes that clear exact.
					current.removeAllRanges();
				}
				return true;
			},
			restore(): boolean {
				const saved = validCapture();
				if (!saved) return false;
				const current = currentSelection(saved);
				if (!current) return false;
				if (!selectionMatchesCapture(current, saved)) {
					try {
						current.setBaseAndExtent(
							saved.anchorNode,
							saved.anchorOffset,
							saved.focusNode,
							saved.focusOffset,
						);
					} catch {
						captured = null;
						return false;
					}
				}
				if (current.isCollapsed || current.toString() !== saved.selectedText) {
					captured = null;
					return false;
				}
				selectionGuardSuspended = false;
				syncNativeSelectionHold();
				return true;
			},
			release(): void {
				captured = null;
				if (activeSelectionGuard === guard) {
					activeSelectionGuard = null;
					selectionGuardSuspended = false;
					syncNativeSelectionHold();
				}
			},
		};
		activeSelectionGuard?.release();
		activeSelectionGuard = guard;
		return guard;
	};
	const prepareLiveInteraction = (): void => {
		selectionGuardEpoch += 1;
		discardActiveSelectionGuardForTransition();
		const selection = displayRef?.ownerDocument.getSelection();
		const ownedSelection = !!selection && paneOwnsSelectionEndpoint(selection);
		// Renderer state moves first: intent, both composed holds, canonical frame,
		// and bottom anchor change as one transition before DOM callbacks can fire.
		const result = renderer?.prepareLiveInteraction();
		// Ownership was captured before reconciliation could detach the old row.
		// Chromium can dispatch the reveal scroll after animation callbacks. Keep
		// the bracket until that scroll arrives; every later admitted input or
		// explicit reader interaction clears it before changing scroll geometry.
		if (ownedSelection) {
			renderer?.beginLiveSelectionRelease();
			selection?.removeAllRanges();
		}
		linkAttachment?.releaseInteraction();
		notifyBackfill(result);
	};
	// Leaving the visible surface ends every reader interval: the selection this
	// pane owned is dropped here, so no reader survives the park. Keeping the
	// interval would freeze the pane on the frame that was current when it left
	// and present that stale grid on its next reveal (the DOM watermark stays
	// behind the canonical one with reconcile_block_reason "reader_pending_frame").
	const releasePaintHolds = (): void => {
		selectionGuardEpoch += 1;
		discardActiveSelectionGuardForTransition();
		const selection = displayRef?.ownerDocument.getSelection();
		const ownedSelection = !!selection && paneOwnsSelectionEndpoint(selection);
		const result = renderer?.prepareLiveInteraction();
		if (ownedSelection) selection?.removeAllRanges();
		linkAttachment?.releaseInteraction();
		notifyBackfill(result);
	};
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
	// Zoom the pane last measured at. Seeded from the current preference so the
	// effect below is purely change-driven — starting at 0 would make its first
	// run look like a change and fire a redundant claim on every cold mount.
	let lastZoomPx = termFontSize();
	let claimSeq = 0;
	let lastEnqueued = { cols: 0, rows: 0 };
	let resizeObs: ResizeObserver | null = null;
	let claimTimer: ReturnType<typeof setTimeout> | null = null;
	let viewportOwner: TerminalViewportOwner | null = null;
	let mountClaim: CellMountClaim | null = null;
	let viewportPositive = false;
	// One intent claim per cold mount: set on any real claim send, so the INITIAL
	// effect no-ops when the inLayout TAB_VISIBLE effect already claimed (each
	// forced its own full snapshot before — worker treats both as intentMount).
	let initialClaimSent = false;
	// Reveal forensics: performance.now() at the inLayout flip → true; the first
	// frame applied afterwards emits diag("cell.reveal") with the elapsed ms.
	let revealT0 = 0;
	const sendTerminalText = (text: string, submit = false): InputAdmission => {
		const payload = text.length === 0 ? new Uint8Array(0) : buildPtyPayload(text, frameBracketed);
		const bytes = submit ? new Uint8Array(payload.byteLength + CR_BYTES.byteLength) : payload;
		if (submit) {
			bytes.set(payload);
			bytes.set(CR_BYTES, payload.byteLength);
		}
		const admission = sendUserTerminalInput(sessionId, bytes);
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

	// A paste of ≥2 newlines into a shell WITHOUT bracketed paste executes every
	// line as it arrives — the classic "pasted a script into bash and it ran half
	// of it" foot-gun. With bracketed paste on, the shell buffers it safely, so
	// there is nothing to warn about and no prompt.
	const MULTILINE_PASTE_MIN_NEWLINES = 2;
	const countLineBreaks = (text: string): number =>
		text.match(/\r\n|\r|\n/g)?.length ?? 0;
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
		await navigator.clipboard.writeText(text).catch(() => { /* denied / insecure ctx */ });
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

	// ── liveness: is the current positive viewport attempt live-ready? ───────
	// Unlike a once-ever frame bit, this drops on every changed attempt and only
	// rises after matching acceptance plus any required authoritative full frame.
	const navigate = useNavigate();
	const [viewportLiveReady, setViewportLiveReady] = createSignal(false);
	const [offline, setOffline] = createSignal(false);
	const retryOffline = () =>
		sendClaim(ResizeCause.TAB_VISIBLE, true);
	const offlineWatch = createOfflineWatch(OFFLINE_GRACE_MS, setOffline, () => {
		diag("cell.offline_retry", { sid: props.session.id });
		retryOffline();
	});
	createEffect(() =>
		offlineWatch.update(
			props.inLayout === true && props.surfaceActive && isPageVisible(),
			viewportLiveReady(),
		),
	);
	onCleanup(() => offlineWatch.dispose());
	const offlineSibling = () =>
		newestOpenSessionForFolderKey(folderKeyOf(props.session), props.session.id);
	const openOfflineSibling = () => {
		const sib = offlineSibling();
		if (sib) navigate(`/s/${sib.id}`);
	};

	// Measure one monospace cell in the display font (probe — independent of
	// rendered content, so a claim can fire before the first frame arrives).
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
		const usableW = Math.max(0, displayRef.clientWidth - padL - padR);
		const usableH = Math.max(0, displayRef.clientHeight - padT - padB);
		return {
			cols: Math.max(1, Math.floor(usableW / cellW)),
			rows: Math.max(1, Math.floor(usableH / cellH)),
		};
	}

	// Zero-size claims remove inactive viewers from the SCD-min PTY size.
	// Hidden, offscreen, and unmounted panes withdraw immediately. Visibility
	// reads route through isPageVisible() so the __smoke.forceVisible automation
	// pin applies.
	function sendWithdraw(): void {
		mountClaim?.deactivate();
		setViewportLiveReady(false);
		viewportPositive = false;
		backfillRef?.suspend();
		if (claimTimer) {
			clearTimeout(claimTimer);
			claimTimer = null;
		}
		lastEnqueued = { cols: 0, rows: 0 };
		const admission = viewportOwner?.claim({
			cols: 0,
			rows: 0,
			cause: ResizeCause.WITHDRAW,
		});
		if (!admission) return;
		diag("cell.claim", {
			sid: sessionId,
			cols: 0,
			rows: 0,
			client_seq: admission.sequence,
		});
	}

	/** Leaving the visible surface releases paint state and the viewport claim. */
	function sendPark(): void {
		releasePaintHolds();
		sendWithdraw();
	}


	// cause = the browser event behind this claim (ResizeCause model).
	// Worker hint only; defaults to VIEWPORT (a plain ResizeObserver tick).
	function sendClaim(
		cause: ResizeCause = ResizeCause.VIEWPORT,
		repairRequired = false,
	): void {
		if (!displayRef || unmounted || !viewportOwner) return;
		if (pending()) return; // placeholder has no PTY yet — don't fire a doomed round-trip
		// Only a pane currently visible on the active terminal surface may claim
		// dimensions. Every other state withdraws immediately.
		if (!isPageVisible() || props.inLayout !== true || !props.surfaceActive) {
			sendPark();
			return;
		}
		const measured = measureViewport();
		if (!measured) return;
		const { cols, rows } = measured;
		// Suppress passive no-change claims. Explicit repair and lifecycle claims
		// still enter the owner so it can reconcile the current Sync generation.
		if (!repairRequired
			&& cause === ResizeCause.VIEWPORT
			&& lastEnqueued.cols > 0
			&& cols === lastEnqueued.cols
			&& rows === lastEnqueued.rows) return;
		lastEnqueued = { cols, rows };
		viewportPositive = true;
		initialClaimSent = true;
		const admission = viewportOwner.claim({
			cols,
			rows,
			cause,
			// The worker skips a repaint when this watermark is current, or sends
			// one authoritative viewport-only full frame when the dormant grid fell behind.
			heldCellSeq: renderer?.heldFrameSeq() ?? 0,
			repairRequired,
		});
		mountClaim?.activate();
		claimSeq = Math.max(claimSeq, Number(admission.sequence));
		diag("cell.claim", {
			sid: sessionId,
			cols,
			rows,
			client_seq: admission.sequence,
		});
	}

	function scheduleClaim(cause: ResizeCause = ResizeCause.VIEWPORT): void {
		if (claimTimer) clearTimeout(claimTimer);
		claimTimer = setTimeout(() => {
			claimTimer = null;
			sendClaim(cause);
		}, CLAIM_DEBOUNCE_MS);
	}

	// Intent-bearing claims (in↔out-of-layout flip, visibilitychange, pageshow)
	// skip the 150ms viewport-wobble debounce — routing them through
	// scheduleClaim added a hard +150ms to every session switch before the
	// worker even started streaming. Clears any pending debounced claim so the
	// immediate send can't be followed by a stale double-send. ResizeObserver
	// claims keep the debounce (that's what it exists for — viewport wobble).
	function sendClaimNow(cause: ResizeCause, repairRequired = false): void {
		if (claimTimer) {
			clearTimeout(claimTimer);
			claimTimer = null;
		}
		sendClaim(cause, repairRequired);
	}


	onMount(() => {
		viewportOwner = acquireTerminalViewportOwner(sessionId);
		mountClaim = acquireCellMountClaim(sessionId);
		const releaseViewportStatus = viewportOwner.subscribeStatus((status) => {
			setViewportLiveReady(viewportPositive && status.status === "ready");
		});
		try {
		// Sync v2 owns connection and replay; no per-pane input channel startup.
		const sid = sessionId;
		const cellOwner = getOwner();
		runWithOwner(cellOwner, () => {
			// ── output: cells ────────────────────────────────────────────────
			renderer = new CellGridRenderer(displayRef!);
		// Retained history is paged only after explicit scroll/find demand; a
		// literal-bottom full frame paints only the current viewport.
		const backfill = createScrollbackBackfill({
			sessionId: props.session.id,
			renderer: () => renderer,
			active: () => props.inLayout === true && props.surfaceActive && isPageVisible(),
		});
		backfillRef = backfill;
		unregisterUserInput = registerUserTerminalInput(sid, prepareLiveInteraction);
		// Only an active surface can produce genuine scrollbar/accessibility
		// intent. Renderer-owned writes remain guarded until their active event.
		const onScroll = () => {
			if (!renderer || !displayRef) return;
			if (props.inLayout !== true || !props.surfaceActive || !isPageVisible()) return;
			notifyBackfill(renderer.handleScroll());
			if (!renderer.atBottom() && renderer.nearHistoryTop()) backfill.onUserScrollUp();
		};
		displayRef!.addEventListener("scroll", onScroll, { passive: true });
		// Predictor is initialized only after the cell handler and first visible
		// claim below, keeping speculative/UI work out of first-paint setup.
		// Ghost cursors: this viewer's latest cursor (sent to others) + the map of
		// OTHER viewers' cursors (received via presence, painted by cellRenderer).
		let lastCurRow = 0,
			lastCurCol = 0;
		// Last cell-frame seq actually APPLIED to the renderer. Frame loss is
		// otherwise invisible: nothing else on this path checks continuity.
		let lastAppliedSeq = 0;
		let lastAppliedGridEpoch: string | null = null;
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
				handler_canonical: {
					grid_epoch: lastAppliedGridEpoch,
					seq: lastAppliedSeq > 0 ? lastAppliedSeq : null,
				},
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
		let awaitingFullFrame = false;
		const ghostMap = new Map<
			string,
			{ x: number; y: number; label?: string }
		>();
		let unsubCell: () => void;
		let unsubPresence: () => void;
		// Input modes ride cell frames directly; the textarea encoder reads these
		// booleans synchronously and never reparses PTY output.
		let measurementRaf = 0;
		runWithOwner(cellOwner, () => {
			const requestFullFrame = (got: number) => {
				if (awaitingFullFrame) return;
				awaitingFullFrame = true;
				signal("cell.seq_gap", {
					sid: sessionId,
					expected: lastAppliedSeq + 1,
					got,
					cooldownKey: sessionId,
				});
				sendClaimNow(ResizeCause.TAB_VISIBLE, true);
			};
			unsubCell = registerCellHandler(props.session.id, (frame) => {
				// Hidden and offscreen panes are unsubscribed. Their next visible
				// claim receives one authoritative full snapshot.
				if (!isPageVisible() || props.inLayout !== true || !props.surfaceActive) return;
				const diagOn = isDiagEnabled();
				const _frameArr = diagOn ? performance.now() : 0;
				// Echo RTT tracker: input→cell-frame round-trip, works even when
				// predictive echo is off. Consumes the last-send timestamp (one
				// measurement per input→echo cycle).
				const sendTs = consumeLastInputSendTs(props.session.id);
				if (sendTs !== undefined) {
					const rttMs = performance.now() - sendTs;
					recordInputRtt(rttMs); // always-on felt-lag ring (leakWatch), independent of diag gate
					if (rttMs > 0 && rttMs < 5000) diag("echo.frame_rtt", { sid: props.session.id, rtt_ms: rttMs });
				}
				if (!renderer) return;
				// Once continuity is lost, only an authoritative full frame can
				// clear the latch. Repeated deltas cannot defer their own repair.
				if (!frame.full) {
					if (awaitingFullFrame) return;
					if (lastAppliedSeq !== 0 && frame.seq !== lastAppliedSeq + 1) {
						requestFullFrame(frame.seq);
						return;
					}
				}
				if (diagOn) {
					diag("cell.apply", {
						sid: props.session.id,
						seq: frame.seq,
						full: frame.full,
						vp_rows: frame.viewportRows.length,
						cursor_vis: frame.cursorVisible,
						cursor_row: frame.cursorRow,
						cursor_col: frame.cursorCol,
					});
				}
				const _ap = diagOn ? performance.now() : 0;
				const applied = frame.full
					? renderer.applyFullFrame(frame)
					: renderer.applyDeltaFrame(frame);
				if (!applied) {
					requestFullFrame(frame.seq);
					return;
				}
				markPhaseOnce("first_cell_apply", props.session.id, {
					sessionId: props.session.id,
					sequence: frame.seq,
					full: frame.full,
				});
				awaitingFullFrame = false;
				lastAppliedSeq = frame.seq;
				lastAppliedGridEpoch = frame.gridEpoch;
				if (frame.full) viewportOwner?.noteFullFrame({
					seq: frame.seq,
					gridEpoch: frame.gridEpoch,
				});
				setAltScreen(frame.altScreen);
				if (diagOn) diag("cell.apply_dur", { sid: props.session.id, dur_ms: performance.now() - _ap });
				if (revealT0 !== 0) {
					// First frame applied after an inLayout reveal — the user-felt
					// switch latency. One grep instead of a week of guessing.
					diag("cell.reveal", { sid: props.session.id, ms: Math.round(performance.now() - revealT0), full: frame.full });
					revealT0 = 0;
				}
				if (diagOn && isPageVisible() && props.inLayout === true && props.surfaceActive) {
					// Two animation frames make this a presentation opportunity, not
					// raster-paint proof. Smoke acceptance uses clipped marker/cursor
					// geometry separately.
					requestAnimationFrame(() => requestAnimationFrame(() => {
						const canonical = renderer?.canonicalEpochSeq() ?? { grid_epoch: null, seq: null };
						const reconciled = renderer?.reconciledEpochSeq() ?? { grid_epoch: null, seq: null };
						diag("cell.dom_reconcile_opportunity", {
							sid: props.session.id,
							dur_ms: performance.now() - _frameArr,
							canonical_epoch: canonical.grid_epoch,
							canonical_seq: canonical.seq,
							reconciled_epoch: reconciled.grid_epoch,
							reconciled_seq: reconciled.seq,
							block_reason: renderer?.reconcileBlockReason() ?? null,
						});
					}));
				}
				frameCursorApp = frame.cursorKeysApp;
				frameBracketed = frame.bracketedPaste;
				frameMouseSgr = frame.mouseSgr;
				frameFocusEvents = frame.focusEvents;
				setMouseTracking(frame.mouseTracking);
				lastCurRow = frame.cursorRow;
				lastCurCol = frame.cursorCol;
				predictor?.onFrame(frame); // reconcile predictions against the authoritative grid
				if (frame.full) backfill.onFullFrame();
			}, () => requestFullFrame(0));
			markPhase("terminal_mount", { sessionId: props.session.id });
			// Optimistic spawn may begin only after renderer + repair-aware cell
			// handler installation. Reserve the mounted slot's real dimensions;
			// one rAF retry covers layout not yet measurable in onMount.
			const publishSpawnMeasurement = (): boolean => {
				if (
					!pending()
					|| props.inLayout !== true
					|| !props.surfaceActive
					|| !isPageVisible()
				) return false;
				const measured = measureViewport();
				if (!measured) return false;
				const measuredClientSeq = claimSeq + 1;
				const published = publishMountedSpawnMeasurement(props.session.id, {
					...measured,
					clientSeq: measuredClientSeq,
				});
				if (published) {
					claimSeq = measuredClientSeq;
					lastEnqueued = measured;
					viewportPositive = true;
					mountClaim?.activate();
				}
				return published;
			};
			if (!publishSpawnMeasurement()) {
				measurementRaf = requestAnimationFrame(() => {
					measurementRaf = 0;
					publishSpawnMeasurement();
				});
			}

			// Persist the ordinary initial claim only after the renderer and cell
			// handler are ready. Optimistic sessions seed this owner on reconcile.
			if (!pending() && props.inLayout === true && props.surfaceActive && isPageVisible()) {
				sendClaim(ResizeCause.INITIAL);
			}

			predictor = new PredictiveEcho(renderer!.predictionHost, {
				mode: predictMode,
				sid: props.session.id,
				onCursor: (col) => renderer?.setPredictedCursor(col),
			});
			createEffect(() => {
				predictMode();
				predictor?.refreshPreference();
			});
			if (typeof localStorage !== "undefined" && localStorage.getItem("roostSmoke") === "1") {
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
					const admission = sendUserTerminalInput(props.session.id, bytes);
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
		const releaseCursorPoll = _registerCursorPoll(() => {
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

		// Heartbeat the latest positive desired geometry, including while its first
		// result is pending. The owner refreshes held seq without allocating a waiter.
		const claimHeartbeat = setInterval(() => {
			if (
				!isPageVisible()
				|| props.inLayout !== true
				|| !props.surfaceActive
				|| !viewportPositive
				|| lastEnqueued.cols <= 0
				|| lastEnqueued.rows <= 0
			) return;
			viewportOwner?.heartbeat(renderer?.heldFrameSeq() ?? 0);
		}, CLAIM_HEARTBEAT_MS);


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
		// overlay, and that same tap's click falls through onto the now-revealed
		// display → onDisplayClick → forceFocus → on-screen keyboard pops, unwanted.
		// Record when this pane becomes active; on touch, the display handlers skip
		// focus within NAV_FALLTHROUGH_MS of activation (the fall-through) but a
		// deliberate later tap still focuses + opens the keyboard. Transition-only
		// (prevActive) so a re-running effect can't keep refreshing the window.
		const NAV_FALLTHROUGH_MS = 700;
		let lastActivatedMs = 0;
		let prevActive = false;
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
			if (isNavFallthrough()) return;
			const t = ev.target as HTMLElement | null;
			if (t?.closest("button, input, textarea, a")) return;
			queueMicrotask(() => inputController?.forceFocus());
		};
		displayRef!.addEventListener("mousedown", onDisplayDown);

		// A trusted click can move focus to body after mousedown. Restore terminal
		// focus after the click settles unless the user has selected text.
		const onDisplayClick = (ev: MouseEvent) => {
			if (ev.button !== 0) return;
			if (isNavFallthrough()) return; // sidebar-tap fall-through on mobile, not a real tap
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
		// Forward pointer and touch gestures ONLY to an application that asked for
		// mouse reporting (frame.mouseTracking, DECSET 1000/1002 read off the core).
		// Alt-screen occupancy is not that question: vim/less/man occupy it without
		// requesting the mouse, and forwarding to them swallowed the click with no
		// native fallback. The user toggle survives as the override for the opposite
		// case — native selection inside an app that DOES want the mouse.
		// Shift or Alt bypasses forwarding for one gesture (see terminalMouse.ts).
		const forwardActive = () => mouseForwardEnabled() && mouseTracking() !== 0;
		const report = (gesture: MouseGesture): boolean => {
			const bytes = terminalMouseReport(
				{ tracking: mouseTracking(), sgr: frameMouseSgr },
				gesture,
			);
			if (bytes === null) return false;
			sendUserTerminalInput(props.session.id, bytes);
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
			const geometry = renderer?.viewportCellGeometry();
			if (geometry) return cellFromPoint(geometry, clientX, clientY);
			if ((cellW === 0 || cellH === 0) && !measureCell())
				return { col: 1, row: 1 };
			// predictionHost IS .cell-viewport — row 0's box, unlike displayRef.
			const origin = (renderer?.predictionHost ?? displayRef!).getBoundingClientRect();
			return {
				col: Math.max(1, 1 + Math.floor((clientX - origin.left) / cellW)),
				row: Math.max(1, 1 + Math.floor((clientY - origin.top) / cellH)),
			};
		};

		let pressedButton: number | null = null;
		let lastMotionCell: { col: number; row: number } | null = null;

		const onWheelForward = (ev: WheelEvent) => {
			if (ev.defaultPrevented || ev.deltaY === 0) return;
			if (!forwardActive()) {
				renderer?.enterReading("wheel");
				return;
			}
			const { col, row } = cellOf(ev.clientX, ev.clientY);
			const forwarded = report({
				kind: ev.deltaY < 0 ? "wheelUp" : "wheelDown",
				col, row,
				shift: ev.shiftKey, alt: ev.altKey, ctrl: ev.ctrlKey, meta: ev.metaKey,
			});
			if (!forwarded) {
				renderer?.enterReading("wheel");
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
			const step = cellH || 18;
			let dy = y - touchY;
			if (Math.abs(dy) < step) return;
			if (!touchForwarding || !forwardActive()) {
				touchY = y;
				renderer?.enterReading("touch");
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

		displayRef!.addEventListener("mousedown", onMouseDownFwd);
		displayRef!.addEventListener("touchstart", onTouchStart, { passive: true });
		displayRef!.addEventListener("touchend", onTouchEnd, { passive: true });
		displayRef!.addEventListener("touchcancel", onTouchEnd, { passive: true });

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

		// Visibility owns both viewport admission and keyboard focus. Parked panes
		// remain mounted but withdraw immediately; reveal reclaims a full repair.
		const claimVisibleFlag = createMemo(
			() => props.inLayout === true && props.surfaceActive,
		);
		createEffect(on(claimVisibleFlag, (visible) => {
			if (!visible) {
				sendPark();
				return;
			}
			if (viewportPositive && initialClaimSent) return;
			revealT0 = performance.now();
			sendClaimNow(ResizeCause.TAB_VISIBLE);
		}));
		// Focus gate: only the focused pane's terminal grabs the keyboard. Touch
		// devices skip it (an explicit tap on the display still focuses) so selecting
		// a pane doesn't pop the on-screen keyboard. This effect is intentionally
		// non-deferred: selection must focus in the same reactive turn.
		const focusGate = createMemo(
			() => claimVisibleFlag() && props.focused === true,
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
				if (!claimVisibleFlag()) return;
				document.addEventListener("selectionchange", onSelectionChange);
				window.addEventListener("pointerup", onSelectionSettled);
				window.addEventListener("keyup", onSelectionSettled);
				window.addEventListener("mousemove", onMouseMoveFwd);
				window.addEventListener("mouseup", onMouseUpFwd);
				document.addEventListener("dragenter", onDragOver);
				document.addEventListener("dragover", onDragOver);
				document.addEventListener("drop", onDrop);
				onCleanup(() => {
					document.removeEventListener("selectionchange", onSelectionChange);
					window.removeEventListener("pointerup", onSelectionSettled);
					window.removeEventListener("keyup", onSelectionSettled);
					window.removeEventListener("mousemove", onMouseMoveFwd);
					window.removeEventListener("mouseup", onMouseUpFwd);
					document.removeEventListener("dragenter", onDragOver);
					document.removeEventListener("dragover", onDragOver);
					document.removeEventListener("drop", onDrop);
				});
			}),
		);

		// Wheel/touchmove passivity: preventDefault (mouse forwarding) only ever
		// fires for an app that REQUESTED mouse reporting — outside that the
		// always-non-passive listeners disabled compositor fast-scroll on every
		// terminal, including every alt-screen pager that never asked for a mouse.
		// Swap on tracking-mode flips. Remove-then-add matters: `passive` isn't part
		// of listener identity, so re-adding the same fn without removing first is
		// silently ignored as a duplicate (the effect's onCleanup runs before each
		// re-run).
		runWithOwner(cellOwner, () =>
			createEffect(() => {
				const passive = mouseTracking() === 0;
				displayRef!.addEventListener("wheel", onWheelForward, { passive });
				displayRef!.addEventListener("touchmove", onTouchMove, { passive });
				onCleanup(() => {
					displayRef?.removeEventListener("wheel", onWheelForward);
					displayRef?.removeEventListener("touchmove", onTouchMove);
				});
			}),
		);

		// Name the grid's log region for assistive tech, and keep it current as the
		// session is renamed or its running program changes.
		runWithOwner(cellOwner, () =>
			createEffect(() => {
				const title = sessionTitle(props.session);
				renderer?.setAccessibleLabel(`Terminal — ${title}`);
				inputController?.setAccessibleLabel(`Terminal input — ${title}`);
			}),
		);

		// Reconcile the coordinator-installed preclaim into the persisted Sync
		// viewport owner before deciding whether a corrective INITIAL is needed.
		createEffect(() => {
			if (pending()) return;
			const seed = takeSpawnViewportSeed(sessionId);
			if (seed) {
				const hasNewerIntent = claimSeq > seed.clientSeq;
				claimSeq = Math.max(claimSeq, seed.effectiveClientSeq);
				if (!hasNewerIntent && viewportOwner) {
					seedTerminalViewportIntent(
						sessionId,
						BigInt(seed.effectiveClientSeq),
						seed.cols,
						seed.rows,
						ResizeCause.INITIAL,
					);
					lastEnqueued = { cols: seed.cols, rows: seed.rows };
					viewportPositive = true;
					const admission = viewportOwner.claim({
						cols: seed.cols,
						rows: seed.rows,
						cause: ResizeCause.INITIAL,
						heldCellSeq: renderer?.heldFrameSeq() ?? 0,
					});
					claimSeq = Math.max(claimSeq, Number(admission.sequence));
					mountClaim?.activate();
					const current = measureViewport();
					initialClaimSent = current?.cols === seed.cols
						&& current.rows === seed.rows;
				}
			}
			if (!claimVisibleFlag() || initialClaimSent) return;
			sendClaim(ResizeCause.INITIAL);
		});
		// Terminal zoom: the cell box changed without the container changing, so
		// nothing else invalidates the measurements. Zero cellW/cellH to force a
		// re-measure, drop the renderer's cached row height (block placeholders and
		// the spacer derive from it), then re-claim. Cols or rows WILL change for a
		// fixed pane, and any size change already takes the full refetch path that
		// CLAUDE.md L11 requires; CLAIM_DEBOUNCE_MS coalesces a key-repeat burst.
		createEffect(() => {
			const px = termFontSize();
			if (px === lastZoomPx) return;
			lastZoomPx = px;
			cellW = 0;
			cellH = 0;
			renderer?.invalidateRowHeight();
			scheduleClaim(ResizeCause.VIEWPORT);
		});
		resizeObs = new ResizeObserver(() => {
			// Re-latch bottom-follow across box changes FIRST — divider drags are
			// continuous height changes and must re-pin per tick even while claims
			// are suppressed (isResizeDragging below).
			notifyBackfill(renderer?.noteBoxResize());
			if (isResizeDragging()) return; // suppress mid-drag PTY round-trips; flush on release (effect below)
			scheduleClaim(ResizeCause.VIEWPORT);
		});
		resizeObs.observe(displayRef!);

		// Divider/sidebar drag: claims are gated off during the drag (resizeObs
		// above). The final size == the last drag frame's size, so the observer
		// does NOT re-fire after commit — fire the single settle claim here on
		// release.
		let wasResizeDragging = false;
		createEffect(() => {
			const dragging = isResizeDragging();
			if (dragging) {
				if (claimTimer) { clearTimeout(claimTimer); claimTimer = null; }
			} else if (wasResizeDragging && props.inLayout !== false && isPageVisible()) {
				sendClaim(ResizeCause.VIEWPORT);
			}
			wasResizeDragging = dragging;
		});

		// Spotlight float/push-back is an INTENT-bearing resize: the pane's slot
		// jumps to the card rect (or back to its tile). Nothing else fires a settle
		// claim for it (the passive ResizeObserver claim doesn't re-fit the PTY on
		// this jump), so force one on the flip. rAF: measure AFTER the slot's new
		// size has laid out.
		const spotlitFlag = createMemo(() => !!props.spotlit);
		let wasSpotlit = false;
		createEffect(() => {
			const s = spotlitFlag();
			if (s === wasSpotlit) return;
			wasSpotlit = s;
			requestAnimationFrame(() => {
				if (claimVisibleFlag() && isPageVisible()) {
					sendClaim(ResizeCause.VIEWPORT);
				}
			});
		});

		// Arrange presets (TerminalDeck doArrange) re-tile every pane at once.
		// Same settle as spotlight above: rAF (commitLayout applies the new slot
		// rects reactively AFTER doArrange returns — a synchronous measure here
		// would read the STALE pre-arrange size). sendClaim no-ops when this
		// pane's size didn't change, so unaffected panes cost nothing; panes newly
		// ENTERING the layout already re-claim exact via the inLayout TAB_VISIBLE
		// effect, and panes LEAVING are gated out by the inLayout check. defer:
		// the INITIAL claim covers mount.
		createEffect(on(arrangeEpoch, () => {
			requestAnimationFrame(() => {
				if (claimVisibleFlag() && isPageVisible()) {
					sendClaim(ResizeCause.VIEWPORT);
				}
			});
		}, { defer: true }));

		// Hidden and offscreen panes withdraw immediately; visibility recovery
		// reclaims one authoritative snapshot on the existing Sync socket.
		const onVisibility = () => {
			if (!claimVisibleFlag() || !isPageVisible()) {
				sendPark();
				return;
			}
			sendClaimNow(ResizeCause.TAB_VISIBLE);
		};
		document.addEventListener("visibilitychange", onVisibility);

		// Hard-exit release: tab close / reload / cross-page nav never runs
		// onCleanup, so without this the worker holds the claim until the TTL
		// reaper and other viewers stay clamped to this tab's size. Best-effort
		// unary (connect-web can't set fetch keepalive) — if the browser cancels
		// it mid-flight, the worker-side freshness tick recovers at
		// VIEWER_CLAIM_FRESH_MS. pageshow re-claims after a bfcache restore
		// (visibilitychange doesn't always fire on restore).
		const onPageHide = () => {
			releasePaintHolds();
			sendWithdraw();
		};
		const onPageShow = () => {
			if (isPageVisible() && claimVisibleFlag()) {
				sendClaimNow(ResizeCause.TAB_VISIBLE);
			}
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
				unsubCell();
				unsubPresence();
				linkAttachment?.dispose();
				linkAttachment = null;
				releaseCursorPoll();
				clearInterval(claimHeartbeat);
				document.removeEventListener("visibilitychange", onVisibility);
				window.removeEventListener("pagehide", onPageHide);
				window.removeEventListener("pageshow", onPageShow);
				document.removeEventListener("keydown", onDocKeydown, true);
				document.removeEventListener("mousedown", onDocMousedown, true);
				sendWithdraw(); // release this viewer's claim immediately on nav-away
				releaseViewportStatus();
				mountClaim?.release();
				viewportOwner?.dispose();
				backfill.dispose();
				backfillRef = null;
				displayRef?.removeEventListener("scroll", onScroll);
				displayRef?.removeEventListener("mousedown", onDisplayDown);
				displayRef?.removeEventListener("click", onDisplayClick);
				displayRef?.removeEventListener("mousedown", onMouseDownFwd);
				displayRef?.removeEventListener("touchstart", onTouchStart);
				displayRef?.removeEventListener("touchend", onTouchEnd);
				displayRef?.removeEventListener("touchcancel", onTouchEnd);
				clearTimeout(claimTimer ?? undefined);
				if (measurementRaf !== 0) cancelAnimationFrame(measurementRaf);

				predictor?.dispose();
				renderer?.dispose();
				unregPreview();
				inputController?.destroy();
				clearInput(sid); // drop the typed-text ring on real unmount
				predictor = null;
				renderer = null;
				inputController = null;
				mountClaim = null;
				viewportOwner = null;
			}),
		);
		});
		} catch (e) {
			unregisterUserInput?.();
			unregisterUserInput = null;
			inputController?.destroy();
			inputController = null;
			sendWithdraw();
			releaseViewportStatus();
			mountClaim?.release();
			viewportOwner?.dispose();
			mountClaim = null;
			viewportOwner = null;
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
			{/* Above the display and inside the pane: the bar consumes real rows, so
          the ResizeObserver re-claims the smaller viewport. Faking or
          compensating that geometry is what CLAUDE.md L11 forbids. */}
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
			{/* Optimistic spawn placeholder: paint the pane instantly; the real
          terminal reconciles into this same tab when the spawn RPC resolves. */}
			<Show when={pending()}>
				<div aria-live="polite" style={{ position: "absolute", inset: "0", display: "flex", "align-items": "center", "justify-content": "center", color: "var(--text-lo)", "font-size": "13px", "pointer-events": "none" }}>
					Starting…
				</div>
			</Show>
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
