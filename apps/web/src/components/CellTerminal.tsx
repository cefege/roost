// CellTerminal — cell-grid model terminal pane (R11, cell mode). Renders OUTPUT via
// CellGridRenderer (pre-rendered cells, never reflows → no history corruption)
// and handles INPUT via a HIDDEN @wterm/dom instance used ONLY as a keystroke
// encoder + terminal-mode oracle.
//
// Why the hidden wterm: keystroke→bytes encoding is mode-dependent — arrows
// (DECCKM ESC[?1h), bracketed paste (ESC[?2004h), keypad — and those modes are
// set by OUTPUT bytes. We feed the byte stream into the hidden wterm so its
// onData encodes correctly in full-screen TUIs, and reuse RoostTerm's
// focus behavior. The hidden wterm reflows internally but is never shown;
// display comes from the cell grid.
//
// The ONLY terminal renderer (byte mode / Terminal.tsx deleted 2026-06-23 — cell
// is canonical). Metadata alongside the cell stream lives here: ghost cursors
// (cellRenderer.setGhosts) and compact OSC 8 mappings (attachTerminalLinks).
// Input rides inputChannel.sendInput (bytes).

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
import { RoostTerm } from "../lib/RoostTerm.ts";
import { CellGridRenderer } from "../lib/cellRenderer.ts";
import { createScrollbackBackfill } from "../lib/scrollbackBackfill.ts";
import { PredictiveEcho } from "../lib/predictiveEcho.ts";
import { TerminalContextMenu } from "./TerminalContextMenu.tsx";
import { pickAndAttachFiles, enqueueAttachment } from "../lib/attachments.ts";
import type { TerminalContext } from "../lib/keytermContext.ts";
import { TerminalComposeButton, activeComposeSessionId } from "./TerminalComposeButton.tsx";
import { TerminalNavButtons } from "./TerminalNavButtons.tsx";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { mouseForwardEnabled } from "../lib/mouseForwardPref.ts";
import { isCompact, isTouchDevice } from "../lib/windowSizeClass.ts";
import { uiStore } from "../store/uiStore.ts";
import { FOCUS_OWNERS } from "../lib/focusOwners.ts";
import { osc8TrackerFor, subscribeOsc8Mappings } from "../lib/terminalOsc8.ts";
import { attachTerminalLinks, type ResolveFile, type TerminalLinkAttachment } from "./terminal-links.ts";
import { downloadWorkerFileByHref } from "../lib/downloadWorkerFile.ts";
import { registerRenderer } from "../lib/terminalPreview.ts";
import {
	registerCellHandler,
	registerPresenceHandler,
} from "../store/sync.ts";
import { inputChannel, consumeLastInputSendTs } from "../ws/input-channel.ts";
import {
	recordInput,
	getInputText,
	clearInput,
} from "../lib/terminalInputHistory.ts";
import { buildPtyPayload, CR_BYTES, enterDelayMs } from "../lib/ptyPaste.ts";
import { applyCtrlModifier } from "../lib/terminalInput.ts";


import { coordClient } from "../connect.ts";
import { ResizeCause } from "@roost/shared/proto/coordinator_pb";
import { isResizeDragging, arrangeEpoch } from "../lib/resizeDrag.ts";
import { diag, isDiagEnabled, signal } from "@roost/shared/diag";
import { getSessionTraceId } from "../lib/diag.ts";
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
import { syncStreamOpen } from "../store/sync-stream-open.ts";
import { newestOpenSessionForFolderKey } from "../store/selectors.ts";
import { folderKeyOf } from "../lib/folderKey.ts";
import { TerminalOfflineNotice } from "./TerminalOfflineNotice.tsx";
import { isPendingSpawn } from "../store/optimisticSpawn.ts";

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
	// Resolve a file path from terminal output → internal file-viewer href.
	// Absolute paths pass through; relative resolve against the session cwd;
	// ~/ paths derive the home dir from cwd (e.g. /Users/you/Code/foo →
	// /Users/you). Feeds clickable file links (Cmd-click → FileViewerSheet).
	const resolveFile: ResolveFile = (rawPath, line) => {
		const cwd = props.session.cwd;
		let abs: string;
		if (rawPath.startsWith("/")) abs = rawPath;
		else if (rawPath.startsWith("~/")) {
			// Derive home dir from cwd (e.g. /Users/you/Code/foo → /Users/you).
			if (!cwd) return null;
			const parts = cwd.split("/");
			if (parts.length < 3) return null;
			abs = "/" + parts[1] + "/" + parts[2] + rawPath.slice(1);
		}
		else if (!cwd) return null;
		else abs = cwd.replace(/\/+$/, "") + "/" + rawPath.replace(/^\.\//, "");
		const enc = abs
			.split("/")
			.map((s) => (s ? encodeURIComponent(s) : s))
			.join("/");
		return `/file/${props.session.worker_fp}/${enc.replace(/^\//, "")}${line ? `#L${line}` : ""}`;
	};

	const attachSelectedFiles = () => pickAndAttachFiles(props.session);

	let displayRef: HTMLDivElement | undefined; // visible — CellGridRenderer paints here
	let inputHostRef: HTMLDivElement | undefined; // hidden — RoostTerm (input + mode oracle)
	const [ctrlArmed, setCtrlArmed] = createSignal(false);

	let renderer: CellGridRenderer | null = null;
	let linkAttachment: TerminalLinkAttachment | null = null;
	let unsubscribeOsc8Mappings: (() => void) | null = null;
	const releasePaintHolds = (): void => {
		const selection = displayRef?.ownerDocument.getSelection();
		const ownsEndpoint = !!selection && (
			(!!selection.anchorNode && !!displayRef?.contains(selection.anchorNode))
			|| (!!selection.focusNode && !!displayRef?.contains(selection.focusNode))
		);
		if (ownsEndpoint) selection.removeAllRanges();
		if (renderer?.setSelectionHold(false)) backfillRef?.onFullFrame();
		linkAttachment?.releaseInteraction();
	};
	// The backfill controller is created inside onMount; the find controller needs
	// it to pull an unpainted match row in before jumping to it.
	let backfillRef: ScrollbackBackfill | null = null;
	let predictor: PredictiveEcho | null = null;
	let term: RoostTerm | null = null;
	let frameBracketed = false; // latest paste mode from cell frames; default matches wterm

	let cellW = 0;
	let cellH = 0;
	// Zoom the pane last measured at. Seeded from the current preference so the
	// effect below is purely change-driven — starting at 0 would make its first
	// run look like a change and fire a redundant claim on every cold mount.
	let lastZoomPx = termFontSize();
	let claimSeq = 0;
	let lastClaimed = { cols: 0, rows: 0 }; // last ADOPTED claim — hold-anchor for VIEWPORT wobble
	let resizeObs: ResizeObserver | null = null;
	let claimTimer: ReturnType<typeof setTimeout> | null = null;
	// Last claim/withdraw actually sent — dedups duplicate hidden/offscreen signals.
	let _lastSent: "claim" | "withdraw" | null = null;
	// One intent claim per cold mount: set on any real claim send, so the INITIAL
	// effect no-ops when the inLayout TAB_VISIBLE effect already claimed (each
	// forced its own full snapshot before — worker treats both as intentMount).
	let initialClaimSent = false;
	// Reveal forensics: performance.now() at the inLayout flip → true; the first
	// frame applied afterwards emits diag("cell.reveal") with the elapsed ms.
	let revealT0 = 0;
	const sendTerminalText = (text: string, submit = false): void => {
		if (text.length === 0) {
			if (submit) inputChannel.sendInput(sessionId, CR_BYTES);
			return;
		}
		inputChannel.sendInput(
			sessionId,
			buildPtyPayload(text, frameBracketed),
		);
		recordInput(sessionId, text);
		if (submit) {
			setTimeout(
				() => inputChannel.sendInput(sessionId, CR_BYTES),
				enterDelayMs(text),
			);
		}
	};

	// A paste of ≥2 newlines into a shell WITHOUT bracketed paste executes every
	// line as it arrives — the classic "pasted a script into bash and it ran half
	// of it" foot-gun. With bracketed paste on, the shell buffers it safely, so
	// there is nothing to warn about and no prompt.
	const MULTILINE_PASTE_MIN_NEWLINES = 2;
	const [pendingPaste, setPendingPaste] = createSignal<string | null>(null);
	const pendingPasteLines = () => {
		const text = pendingPaste();
		return text === null ? 0 : text.split("\n").length;
	};

	/** The single entry point for every clipboard-originated paste: the ⌘⇧V
	 *  chord, the context menu, and a native paste we intercepted. */
	function pasteText(text: string): void {
		if (text.length === 0) return;
		const newlines = text.split("\n").length - 1;
		if (newlines >= MULTILINE_PASTE_MIN_NEWLINES && !frameBracketed) {
			setPendingPaste(text);
			return;
		}
		sendTerminalText(text);
	}

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
	// Native browser scrolling owns the terminal position. CellGridRenderer only
	// conditionally pins new output when a render began at the literal bottom.
	// This component does not classify, restore, or otherwise write scrollTop.
	const [altScreen, setAltScreen] = createSignal(false); // tracks frame.altScreen — gates mouse/touch forwarding + wheel passivity

	// True while this session is an optimistic placeholder (spawn RPC in flight).
	// For a non-optimistic session this is always false → every gate below is a
	// no-op, so mount behaviour is byte-identical to before.
	const pending = createMemo(() => isPendingSpawn(props.session.id));

	// ── liveness: does this VIEWED pane actually receive frames? ──────────
	// A live pane paints a snapshot within a beat of being claimed; a dead
	// breadcrumb never does. offlineWatch turns that silent-blank state into an
	// explicit notice. hasFrame flips true on the first frame (below) and never
	// back — a session that ever painted is proven live.
	const navigate = useNavigate();
	const [hasFrame, setHasFrame] = createSignal(false);
	const [offline, setOffline] = createSignal(false);
	const retryOffline = () =>
		sendClaim(ResizeCause.TAB_VISIBLE);
	const offlineWatch = createOfflineWatch(OFFLINE_GRACE_MS, setOffline, () => {
		diag("cell.offline_retry", { sid: props.session.id });
		retryOffline();
	});
	createEffect(() =>
		offlineWatch.update(
			props.inLayout === true && props.surfaceActive && isPageVisible(),
			hasFrame(),
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

	// Zero-size claims remove inactive viewers from the SCD-min PTY size.
	// Hidden, offscreen, and unmounted panes withdraw immediately. Visibility
	// reads route through isPageVisible() so the __smoke.forceVisible automation
	// pin applies.
	function sendWithdraw(): void {
		backfillRef?.suspend();
		if (claimTimer) {
			clearTimeout(claimTimer);
			claimTimer = null;
		}
		if (_lastSent === "withdraw") return; // already withdrawn — skip the duplicate RPC
		_lastSent = "withdraw";
		lastClaimed = { cols: 0, rows: 0 }; // re-show must re-claim, not be held by a stale anchor
		claimSeq += 1;
		diag("cell.claim", {
			sid: props.session.id,
			cols: 0,
			rows: 0,
			client_seq: claimSeq,
		});
		void coordClient.sessionsResize({
			sessionId: props.session.id,
			cols: 0,
			rows: 0,
			clientSeq: BigInt(claimSeq),
			cause: ResizeCause.WITHDRAW,
		});
	}

	/** Leaving the visible surface releases paint state and the viewport claim. */
	function sendPark(): void {
		releasePaintHolds();
		sendWithdraw();
	}


	// cause = the browser event behind this claim (ResizeCause model).
	// Worker hint only; defaults to VIEWPORT (a plain ResizeObserver tick).
	function sendClaim(cause: ResizeCause = ResizeCause.VIEWPORT): void {
		if (!displayRef || unmounted) return;
		if (pending()) return; // placeholder has no PTY yet — don't fire a doomed round-trip
		// Only a pane currently visible on the active terminal surface may claim
		// dimensions. Every other state withdraws immediately.
		if (!isPageVisible() || props.inLayout !== true || !props.surfaceActive) {
			sendPark();
			return;
		}
		if (cellW === 0 || cellH === 0) {
			if (!measureCell()) return;
		}
		const cs = getComputedStyle(displayRef);
		const padL = parseFloat(cs.paddingLeft) || 0;
		const padR = parseFloat(cs.paddingRight) || 0;
		const padT = parseFloat(cs.paddingTop) || 0;
		const padB = parseFloat(cs.paddingBottom) || 0;
		const usableW = Math.max(0, displayRef.clientWidth - padL - padR);
		const usableH = Math.max(0, displayRef.clientHeight - padT - padB);
		const cols = Math.max(1, Math.floor(usableW / cellW));
		const rows = Math.max(1, Math.floor(usableH / cellH));
		// Suppress no-change claims to avoid needless resize round-trips.
		if (cause === ResizeCause.VIEWPORT && lastClaimed.cols > 0
			&& cols === lastClaimed.cols && rows === lastClaimed.rows) return;
		lastClaimed = { cols, rows };
		_lastSent = "claim";
		initialClaimSent = true;
		claimSeq += 1;
		diag("cell.claim", {
			sid: props.session.id,
			cols,
			rows,
			client_seq: claimSeq,
		});
		void coordClient.sessionsResize({
			sessionId: props.session.id,
			cols,
			rows,
			clientSeq: BigInt(claimSeq),
			cause,
			// The worker skips a repaint when this watermark is current, or sends
			// one authoritative viewport-only full frame when the dormant grid fell behind.
			heldCellSeq: renderer?.heldFrameSeq() ?? 0,
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
	function sendClaimNow(cause: ResizeCause): void {
		if (claimTimer) {
			clearTimeout(claimTimer);
			claimTimer = null;
		}
		sendClaim(cause);
	}


	onMount(async () => {
		try {
		inputChannel.start();
		const sid = props.session.id; // body-scope capture for onCleanup (L11)
		// Owner captured BEFORE the `await term.init()` below. onCleanup after an
		// await registers against a null owner (Solid drops it) → the teardown block
		// never runs → document drop/paste/mousedown/keydown listeners + intervals
		// leak per unmount. Leaked stale `onDrop` closures pass their isActive guard
		// (disposed memo's last value) → N× enqueueAttachment on one drop. Bind the
		// post-await onCleanup to this owner so teardown actually fires on unmount.
		const cellOwner = getOwner();

		// The keystroke encoder's WASM instantiate is started here but NOT awaited:
		// the browser wterm is renderless (RoostTerm.ts) and is only an input
		// oracle, so nothing about painting the grid needs it. Awaiting it put a
		// wasm compile ahead of the renderer, the cell subscription and the first
		// viewport claim on every pane mount. `term` stays null until init lands,
		// which is what makes every `term?.` call site below a safe no-op until
		// then (syncInputModes checks it explicitly). The continuation is attached
		// further down, once syncInputModes and the input wiring it needs exist.
		const initializedTerm = new RoostTerm(inputHostRef!, {
			autoFocus: props.focused === true && !isTouchDevice() && activeComposeSessionId() === null,
			renderless: true,
		});
		const termReady = initializedTerm.init();
		runWithOwner(cellOwner, () => {
			// ── output: cells ────────────────────────────────────────────────
			renderer = new CellGridRenderer(displayRef!);
		const unregPreview = registerRenderer(props.session.id, renderer);
		// Retained history is paged only after explicit scroll/find demand; a
		// literal-bottom full frame paints only the current viewport.
		const backfill = createScrollbackBackfill({
			sessionId: props.session.id,
			renderer: () => renderer,
			active: () => props.inLayout === true && props.surfaceActive && isPageVisible(),
		});
		backfillRef = backfill;
		// Programmatic bottom pins can emit scroll events. Only an off-bottom
		// reader near the painted history top constitutes demand.
		const onScroll = () => {
			if (!renderer || !displayRef) return;
			if (props.inLayout === false || !isPageVisible()) return;
			if (renderer.releaseReaderFreezeAtBottom()) {
				backfill.onFullFrame();
				return;
			}
			if (!renderer.atBottom() && renderer.nearHistoryTop()) backfill.onUserScrollUp();
		};
		displayRef!.addEventListener("scroll", onScroll, { passive: true });
		// Predictive local echo — speculative client overlay, gated on
		// SRTT + alt-screen; no-op on a fast link. See lib/predictiveEcho.ts.
		predictor = new PredictiveEcho(renderer.predictionHost, {
			sid: props.session.id,
			onCursor: (col) => renderer?.setPredictedCursor(col),
		});
		// Debug bridge (Step-1 diagnostic lever): behind the existing smoke flag so
		// production `window` stays clean. The last-mounted pane re-binds the
		// global, so focus the pane under test; on dispose, `predictor = null`
		// makes the closure return null. Returns counts/SRTT/epoch/mode only — no
		// session content.
		if (typeof localStorage !== "undefined" && localStorage.getItem("roostSmoke") === "1") {
			(window as Window & { __roostPredictDebug?: () => unknown }).__roostPredictDebug =
				() => predictor?._debug() ?? null;
		}
		// Ghost cursors: this viewer's latest cursor (sent to others) + the map of
		// OTHER viewers' cursors (received via presence, painted by cellRenderer).
		let lastCurRow = 0,
			lastCurCol = 0;
		// Last cell-frame seq actually APPLIED to the renderer. Frame loss is
		// otherwise invisible: nothing else on this path checks continuity.
		let lastAppliedSeq = 0;
		let awaitingFullFrame = false;
		const ghostMap = new Map<
			string,
			{ x: number; y: number; label?: string }
		>();
		let unsubCell: () => void;
		let unsubPresence: () => void;
		// Input-encoding modes now ride the cell frame (cursorKeysApp / bracketedPaste)
		// instead of re-parsing the whole output byte stream through the hidden wterm.
		// We keep the hidden wterm ONLY as the keystroke encoder; write the tiny mode-set
		// escapes when a flag flips so its onData encodes arrows (DECCKM) + paste correctly.
		let frameCursorApp = false; // latest cursor-key mode from cell frames
		let syncedCursorApp = false, syncedBracketed = false; // what the hidden wterm knows (wterm defaults: both off)
		const syncInputModes = () => {
			if (!term) return;
			if (frameCursorApp !== syncedCursorApp) { syncedCursorApp = frameCursorApp; term.write(frameCursorApp ? "\x1b[?1h" : "\x1b[?1l"); }
			if (frameBracketed !== syncedBracketed) { syncedBracketed = frameBracketed; term.write(frameBracketed ? "\x1b[?2004h" : "\x1b[?2004l"); }
		};
		runWithOwner(cellOwner, () => {
			const requestFullFrame = (got: number) => {
				if (awaitingFullFrame) return;
				awaitingFullFrame = true;
				signal("cell.seq_gap", {
					sid: props.session.id,
					expected: lastAppliedSeq + 1,
					got,
					cooldownKey: props.session.id,
				});
				sendClaimNow(ResizeCause.TAB_VISIBLE);
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
				if (!renderer.apply(frame)) {
					requestFullFrame(frame.seq);
					return;
				}
				awaitingFullFrame = false;
				setHasFrame(true);
				lastAppliedSeq = frame.seq;
				setAltScreen(frame.altScreen);
				if (diagOn) diag("cell.apply_dur", { sid: props.session.id, dur_ms: performance.now() - _ap });
				if (revealT0 !== 0) {
					// First frame applied after an inLayout reveal — the user-felt
					// switch latency. One grep instead of a week of guessing.
					diag("cell.reveal", { sid: props.session.id, ms: Math.round(performance.now() - revealT0), full: frame.full });
					revealT0 = 0;
				}
				if (diagOn && isPageVisible() && props.inLayout === true && props.surfaceActive) {
					requestAnimationFrame(() => requestAnimationFrame(() => {
						diag("cell.paint_screen", { sid: props.session.id, dur_ms: performance.now() - _frameArr });
					}));
				}
				frameCursorApp = frame.cursorKeysApp;
				frameBracketed = frame.bracketedPaste;
				syncInputModes();
				lastCurRow = frame.cursorRow;
				lastCurCol = frame.cursorCol;
				predictor?.onFrame(frame); // reconcile predictions against the authoritative grid
				if (frame.full) backfill.onFullFrame();
			});

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
					f.kind === "presence-leave" &&
					typeof f.viewer_id === "string"
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

		// Heartbeats refresh only the active, visible pane; dormant panes withdraw.
		const claimHeartbeat = setInterval(() => {
			if (
				!isPageVisible()
				|| props.inLayout !== true
				|| !props.surfaceActive
				|| lastClaimed.cols <= 0
				|| lastClaimed.rows <= 0
			) return;
			void coordClient.sessionsResize({
				sessionId: props.session.id,
				cols: lastClaimed.cols,
				rows: lastClaimed.rows,
				clientSeq: BigInt(claimSeq),
				cause: ResizeCause.HEARTBEAT,
				heldCellSeq: renderer?.heldFrameSeq() ?? 0,
			});
		}, CLAIM_HEARTBEAT_MS);

		// Init continuation: publish `term`, take focus, and wire the encoder. A
		// keystroke arriving before this lands is simply not encoded — the pane
		// cannot be focused before the encoder exists, and forceFocus runs here.
		void termReady.then(() => runWithOwner(cellOwner, () => {
			if (unmounted) { initializedTerm.destroy(); return; }
			term = initializedTerm;
			if (
				props.inLayout === true
				&& props.focused === true
				&& !isTouchDevice()
				&& activeComposeSessionId() === null
			) {
				initializedTerm.forceFocus();
				requestAnimationFrame(() => {
					if (
						!unmounted
						&& props.inLayout === true
						&& props.focused === true
						&& activeComposeSessionId() === null
					) initializedTerm.forceFocus();
				});
			}
			syncInputModes(); // flush modes seen before the hidden wterm existed
			initializedTerm.onData = (data: string) => {
				const armed = ctrlArmed();
				const controlledData = armed ? applyCtrlModifier(data) : data;
				if (armed) setCtrlArmed(false);
				const bytes = new TextEncoder().encode(controlledData);
				predictor?.predict(bytes); // speculative echo before the round-trip
				inputChannel.sendInput(props.session.id, bytes);
				recordInput(props.session.id, controlledData); // typed text → keyterm context
			};
		})).catch((e: unknown) => {
			// Without this the encoder's rejection is an unhandled rejection and the
			// pane silently cannot accept input (the "can't input anything" class).
			initializedTerm.destroy();
			term = null;
			signal("diag.corruption_signal", {
				kind: "cell_mount_failed",
				sid: props.session.id,
				session_trace_id: getSessionTraceId(props.session.id),
				msg: String(e),
				cooldownKey: props.session.id,
			});
		});

		// OSC 8 mappings are retained in sync-dispatch before a pane is visited.
		const osc8 = osc8TrackerFor(props.session.id);
		// Linkify rendered .cell-row text (OSC 8 links + regex URLs), Cmd/Ctrl-gated.
		linkAttachment = attachTerminalLinks(displayRef!, osc8, {
			resolveFile,
			// Cmd/Ctrl-click a file path → download it from the worker (works whether
			// the session is local or on another Mac), replacing the in-app viewer.
			onOpenFile: (href) => void downloadWorkerFileByHref(href),
			githubOwnerRepo: () => props.session.git_remote ?? undefined,
			// Freeze cell repaints while Cmd-hovering a link so the wrapped <a> stops
			// churning under the cursor (the pointer↔text flicker).
			onArmedHoverChange: (active) => {
				if (renderer?.setArmedHold(active)) backfill.onFullFrame();
			},
		});
		unsubscribeOsc8Mappings = subscribeOsc8Mappings(props.session.id, () => {
			linkAttachment?.refresh();
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

		// Clicks land on visible cell rows, not the off-screen textarea — bridge
		// them to the hidden wterm's focus (RoostTerm's own bridge is on the
		// hidden host, which never receives these clicks).
		const onDisplayDown = (ev: MouseEvent) => {
			// Left-click only: right-click (button 2) opens the context menu and
			// must NOT steal focus — forceFocus collapses the window selection, and
			// its microtask runs before `contextmenu` reads getSelection(), so the
			// "Copy" item would never see the marked text.
			if (ev.button !== 0) return;
			if (isNavFallthrough()) return;
			const t = ev.target as HTMLElement | null;
			if (t?.closest("button, input, textarea, a")) return;
			queueMicrotask(() => term?.forceFocus());
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
			term?.forceFocus();
		};
		displayRef!.addEventListener("click", onDisplayClick);

		// Renderer DOM replacement would otherwise destroy an active selection.
		// Hold renderer writes while selection exists, then flush the latest frame.
		const onSelectionChange = () => {
			const sel = displayRef?.ownerDocument.getSelection();
			const held =
				!!sel &&
				!sel.isCollapsed &&
				sel.rangeCount > 0 &&
				!!sel.anchorNode &&
				displayRef!.contains(sel.anchorNode) &&
				!!sel.focusNode &&
				displayRef!.contains(sel.focusNode);
			if (renderer?.setSelectionHold(held)) backfill.onFullFrame();
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

		// ── mouse / touch forwarding (SGR-1006 mouse-tracking) ───────────────
		// Forward pointer and touch gestures to an alternate-screen TUI as mouse
		// events. A plain shell keeps native browser selection and DOM scroll.
		// Shift or Alt bypasses forwarding for native selection.
		const forwardActive = () => mouseForwardEnabled() && altScreen();
		const modifierBypass = (ev: MouseEvent | WheelEvent) =>
			ev.shiftKey || ev.altKey;
		const sendSeq = (seq: string) =>
			inputChannel.sendInput(props.session.id, new TextEncoder().encode(seq));
		const cellOf = (
			clientX: number,
			clientY: number,
		): { col: number; row: number } => {
			if ((cellW === 0 || cellH === 0) && !measureCell())
				return { col: 1, row: 1 };
			const rect = displayRef!.getBoundingClientRect();
			return {
				col: Math.max(1, 1 + Math.floor((clientX - rect.left) / cellW)),
				row: Math.max(1, 1 + Math.floor((clientY - rect.top) / cellH)),
			};
		};

		let pressedButton: number | null = null;
		let lastMotionCell: { col: number; row: number } | null = null;

		const onWheelForward = (ev: WheelEvent) => {
			if (!forwardActive() || modifierBypass(ev)) return;
			const { col, row } = cellOf(ev.clientX, ev.clientY);
			sendSeq(`\x1b[<${ev.deltaY < 0 ? 64 : 65};${col};${row}M`);
			ev.preventDefault();
		};
		const onMouseDownFwd = (ev: MouseEvent) => {
			if (!forwardActive() || modifierBypass(ev)) return;
			// Modified link clicks open locally through the native anchor; never
			// forward them to the worker PTY.
			if ((ev.target as HTMLElement | null)?.closest("a")) return;
			// Middle button is reserved for the deck's bring-to-front toggle
			// (TerminalDeck onDeckPointerDown) — never forwarded as SGR press.
			if (ev.button === 1) return;
			const button = ev.button === 1 ? 1 : ev.button === 2 ? 2 : 0;
			const { col, row } = cellOf(ev.clientX, ev.clientY);
			pressedButton = button;
			lastMotionCell = { col, row };
			sendSeq(
				`\x1b[<${button | (ev.metaKey ? 8 : 0) | (ev.ctrlKey ? 16 : 0)};${col};${row}M`,
			);
		};
		const onMouseMoveFwd = (ev: MouseEvent) => {
			if (pressedButton === null || !forwardActive()) return;
			const { col, row } = cellOf(ev.clientX, ev.clientY);
			if (
				lastMotionCell &&
				lastMotionCell.col === col &&
				lastMotionCell.row === row
			)
				return;
			lastMotionCell = { col, row };
			sendSeq(
				`\x1b[<${pressedButton | 32 | (ev.metaKey ? 8 : 0) | (ev.ctrlKey ? 16 : 0)};${col};${row}M`,
			);
		};
		const onMouseUpFwd = (ev: MouseEvent) => {
			if (pressedButton === null) return;
			const button = pressedButton;
			pressedButton = null;
			lastMotionCell = null;
			if (!forwardActive()) return;
			const { col, row } = cellOf(ev.clientX, ev.clientY);
			sendSeq(
				`\x1b[<${button | (ev.metaKey ? 8 : 0) | (ev.ctrlKey ? 16 : 0)};${col};${row}m`,
			);
		};

		// Touch forwarding translates finger travel to terminal wheel events while
		// alternate-screen forwarding is active; otherwise DOM scrolling remains native.
		let touchY: number | null = null;
		let touchCol = 1,
			touchRow = 1;
		const onTouchStart = (ev: TouchEvent) => {
			if (!forwardActive() || ev.touches.length !== 1) {
				touchY = null;
				return;
			}
			const t = ev.touches[0]!;
			touchY = t.clientY;
			const c = cellOf(t.clientX, t.clientY);
			touchCol = c.col;
			touchRow = c.row;
		};
		const onTouchMove = (ev: TouchEvent) => {
			if (!altScreen()) return; // native scroll owns the movement; never preventDefault outside alt-screen
			if (touchY === null || !forwardActive() || ev.touches.length !== 1)
				return;
			const y = ev.touches[0]!.clientY;
			const step = cellH || 18; // original per-cell-height speed
			let dy = y - touchY;
			while (Math.abs(dy) >= step) {
				const up = dy > 0; // finger moved down → scroll up (history)
				sendSeq(`\x1b[<${up ? 64 : 65};${touchCol};${touchRow}M`);
				dy -= up ? step : -step;
			}
			touchY = y - dy; // carry sub-notch remainder
			ev.preventDefault(); // suppress native scroll only while forwarding
		};
		const onTouchEnd = () => { touchY = null; };

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
		const enqueueFileItems = (items: DataTransferItemList | null | undefined) => {
			if (!items) return;
			for (let i = 0; i < items.length; i++) {
				const item = items[i]!;
				if (item.kind !== "file") continue;
				const file = item.getAsFile();
				if (file) void enqueueAttachment(props.session, file);
			}
		};
		const onDragOver = (e: DragEvent) => {
			if (!props.focused || !isPageVisible() || !dragHasFiles(e)) return;
			e.preventDefault(); // allow the drop + stop the browser opening the file
		};
		const onDrop = (e: DragEvent) => {
			if (!props.focused || !isPageVisible() || !dragHasFiles(e)) return;
			e.preventDefault();
			enqueueFileItems(e.dataTransfer?.items);
		};
		const onPaste = (e: ClipboardEvent) => {
			enqueueFileItems(e.clipboardData?.items);
			// A native paste otherwise lands in wterm's hidden textarea and rides
			// onData straight to the PTY, which is where the unbracketed-multiline
			// foot-gun actually happens — so route a risky payload through the same
			// confirmation path the ⌘⇧V chord uses.
			const text = e.clipboardData?.getData("text") ?? "";
			if (!frameBracketed && text.split("\n").length - 1 >= MULTILINE_PASTE_MIN_NEWLINES) {
				e.preventDefault();
				pasteText(text);
			}
		};
		// The display never has focus (wterm's textarea does, off-screen in
		// inputHostRef), so the paste event fires there — listen on both.
		displayRef!.addEventListener("paste", onPaste);
		inputHostRef!.addEventListener("paste", onPaste);

		// Focus the hidden textarea whenever this pane is the active one. NON-
		// deferred (mirrors Terminal.tsx ~L945) so it fires on mount too: the prior
		// { defer: true } variant skipped the initial run, so a pane that mounts
		// already-active never got focused → focus stuck on <body> → every keystroke
		// dropped. THIS was the cell-phase-3b "can't input in cell mode" bug. Created
		// inside onMount so `term` is already assigned when it first runs.
		// Only a pane on the visible terminal surface owns a viewport claim.
		// Offscreen sessions stay mounted with their last grid but withdraw
		// immediately; reveal reclaims and receives an authoritative snapshot.
		// Memoizing this boolean avoids reclaims on unrelated layout object churn.
		const claimVisibleFlag = createMemo(
			() => props.inLayout === true && props.surfaceActive,
		);
		createEffect(on(claimVisibleFlag, (visible) => {
			if (!visible) {
				sendPark();
				return;
			}
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
			if (!isTouchDevice() && activeComposeSessionId() === null) term?.forceFocus();
		});


		// Per-pane GLOBAL listeners (window/document) attach only while this pane
		// is IN the layout: the deck keeps every open session mounted, so the old
		// unconditional attach scaled N handlers per pointer/selection/drag event
		// app-wide. runWithOwner(cellOwner): post-await effects have no owner, and
		// this one MUST dispose on unmount or the listeners leak. Cleanup closes
		// over locals only (L11 feedback_no_props_read_in_oncleanup).
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

		// Wheel/touchmove passivity: preventDefault (mouse-forwarding) only ever
		// fires in alt-screen — outside it the always-non-passive listeners
		// disabled compositor fast-scroll on every terminal. Swap on altScreen
		// flips. Remove-then-add matters: `passive` isn't part of listener
		// identity, so re-adding the same fn without removing first is silently
		// ignored as a duplicate (the effect's onCleanup runs before each re-run).
		runWithOwner(cellOwner, () =>
			createEffect(() => {
				const passive = !altScreen();
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
				renderer?.setAccessibleLabel(`Terminal — ${sessionTitle(props.session)}`);
			}),
		);

		// ── viewport claim — drives PTY/grid size; worker emits a full cell
		//    snapshot on claim so the pane paints immediately. sendClaim()
		//    self-measures on first call (cellW/cellH still 0). ──────────────
		// Fire the INITIAL claim once the session is real. Non-optimistic: now
		// (pending already false) — identical timing to the old unconditional call.
		// Optimistic: when the spawn confirms and pending flips false. Bare
		// createEffect post-await matches the inLayout/focus effects above (tracks
		// reactively, re-runs on the flip); initialClaimSent (component scope, set
		// on any real claim send) guarantees exactly one intent claim per cold
		// mount — the inLayout TAB_VISIBLE effect usually wins and this no-ops.
		createEffect(() => {
			if (pending() || !claimVisibleFlag()) return;
			if (initialClaimSent) return;
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
			renderer?.noteBoxResize();
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
		// Re-claim whenever the firehose WS (re)opens. Cell content is live-forward
		// only (not in sinceEventId backfill), so a reconnect drops the grid stale
		// until the worker re-emits a snapshot — which only claimViewport triggers.
		// This is the recovery for a genuine reconnect; an ordinary refocus keeps
		// its socket (sync-bootstrap.ts::shouldRedialOnRefocus) and lands its
		// repaint through onVisibility's claim, which no longer races a close.
		createEffect(on(syncStreamOpen, (open) => {
			if (!open || !claimVisibleFlag() || !isPageVisible()) return;
			sendClaimNow(ResizeCause.TAB_VISIBLE);
		}, { defer: true }));

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

		// ── FOCUS SAFETY NET (the "sometimes can't type, refresh/switch fixes it"
		//    bug) ──────────────────────────────────────────────────────────────
		// wterm's textarea lives offscreen; clicking any bare control (sidebar
		// ✕/FAB, mic, nav-keys, launch FAB, tabs) parks focus ON the control →
		// onData stops firing → keystrokes vanish until remount/pane-switch. Two
		// capture-phase nets on this active+visible pane keep that from happening:
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
		// (2) keydown RECOVER — backstop for focus an overlay grabbed
		//     programmatically (⌘K palette, rename dialog) and then dropped, where
		//     no mousedown fired. Recovers on a real typing key when focus is parked
		//     nowhere (body/html) or on a bare control; leaves text widgets/overlays
		//     and Space/Enter (button activation) alone. inputHostRef short-circuit
		//     keeps the per-keystroke hot path off the closest() walk.
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
			if (inputHostRef?.contains(ae)) return; // already typing into this terminal
			if (ae === document.body || ae === document.documentElement) {
				if (e.metaKey || e.altKey || e.isComposing) return;
				if (e.key === "Control" || e.key === "Shift") return;

				term?.forceFocus();
				if (term?.dispatchKeydown(e.key, {
					code: e.code,
					location: e.location,
					repeat: e.repeat,
					ctrlKey: e.ctrlKey,
					shiftKey: e.shiftKey,
					altKey: e.altKey,
					metaKey: e.metaKey,
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
			term?.forceFocus();
		};
		document.addEventListener("keydown", onDocKeydown, true);

		runWithOwner(cellOwner, () =>
			onCleanup(() => {
				releasePaintHolds();
				unsubCell();
				unsubPresence();
				unsubscribeOsc8Mappings?.();
				unsubscribeOsc8Mappings = null;
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
				backfill.dispose();
				displayRef?.removeEventListener("scroll", onScroll);
				displayRef?.removeEventListener("mousedown", onDisplayDown);
				displayRef?.removeEventListener("click", onDisplayClick);
				displayRef?.removeEventListener("mousedown", onMouseDownFwd);
				displayRef?.removeEventListener("touchstart", onTouchStart);
				displayRef?.removeEventListener("touchend", onTouchEnd);
				displayRef?.removeEventListener("touchcancel", onTouchEnd);
				displayRef?.removeEventListener("paste", onPaste);
				inputHostRef?.removeEventListener("paste", onPaste);
				clearTimeout(claimTimer ?? undefined);

				predictor?.dispose();
				renderer?.dispose();
				unregPreview();
				term?.destroy();
				clearInput(sid); // drop the typed-text ring on real unmount
				predictor = null;
				renderer = null;
				term = null;
			}),
		);
		});
		} catch (e) {
			term?.destroy();
			term = null;
			// A renderer/term init throw here is an unhandled rejection = silent
			// dead pane (the "can't input" class). Surface it as a corruption signal.
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
					onDismiss={() => { find.closeFind(); term?.forceFocus(); }}
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
			{/* Off-screen, NOT aria-hidden: Chrome blocks programmatic focus on
          descendants of aria-hidden subtrees → the textarea must stay reachable
          or input dies. Hidden from sight via -99999px is enough. */}
			<div
				ref={inputHostRef}
				style={{
					position: "absolute",
					left: "-99999px",
					top: "0",
					width: "240px",
					height: "120px",
					overflow: "hidden",
				}}
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
					onKey={(key: string) => { term?.dispatchKeydown(key); }}
					ctrlArmed={ctrlArmed()}
					onCtrlArmedChange={(armed: boolean) => {
						if (armed && !isTouchDevice()) term?.forceFocus();
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
				/>
			</Show>
			<TerminalContextMenu
				session={props.session}
				getContainer={() => displayRef ?? null}
				onAttachFile={attachSelectedFiles}
				onPasteText={sendTerminalText}

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
