// Controller intents → the app's EXISTING focus/menu/deck machinery. Directional
// and activate/back intents become one untrusted synthetic key on the focused
// element, so every roving surface that already owns arrows/Enter/Escape keeps
// owning them; only the two things an untrusted key cannot do (native click
// activation, native scrolling) are compensated explicitly here.
// Callers: App.tsx (installGamepadSource(runPadActions)), UiBridge (router io).
// Depends on: lib/{deckOps,padBindings,padFolders,folderGroups,padMode,
// keyboardShortcuts,terminalReaderScroll,voiceState}, TerminalNavButtons,
// store/{paneLayout*,selectors,uiStore,toastStore}.

import { createSignal } from "solid-js";
import { diag } from "@roost/observability/diag";
import { allLeaves, type Layout, type PaneLeaf } from "../store/paneLayout.ts";
import { resolveLayout } from "../store/paneLayoutStore.ts";
import { activeSessionForPath, liveSessionIdsForFolder } from "../store/selectors.ts";
import { addToast } from "../store/toastStore.ts";
import { closeSidebar, uiStore } from "../store/uiStore.ts";
import {
	closeTerminalNavPad, focusTerminalNavPadFirstKey, terminalNavPadOpen,
	toggleTerminalNavPad,
} from "../components/TerminalNavButtons.tsx";
import {
	deckOpsCtxForFolder, focusPaneOp, selectTabOp, spotlitPaneIdIn,
	type DeckOpsCtx,
} from "./deckOps.ts";
import { buildFolderGroups } from "./folderGroups.ts";
import { folderKeyOf } from "./folderKey.ts";
import {
	closeCmdPalette, closeControllerMap, cmdPaletteOpen, controllerMapOpen,
	helpOpen, openCmdPalette, openControllerMap,
} from "./keyboardShortcuts.ts";
import type { PadAction, PadHintContext } from "./padBindings.ts";
import { nextFolderSessionId } from "./padFolders.ts";
import { padModeActive } from "./padMode.ts";
import { PAD_SCROLL_STEP_PX, scrollTerminalReaderBox } from "./terminalReaderScroll.ts";
import { voiceControls, voiceDictating } from "./voiceState.ts";

export interface PadRouterIo {
	navigate: (href: string) => void;
	getPath: () => string;
}

/** The legend hides this long after the last controller input. */
const PAD_HINT_IDLE_MS = 4000;

const [hintsVisible, setHintsVisible] = createSignal(false);
const [hintContext, setHintContext] = createSignal<PadHintContext>("default");

export const padHintsVisible = hintsVisible;
export const padHintContext = hintContext;

/** UiBridge injects router navigation; the pad module stays router-free. */
export function setPadRouterIo(io: PadRouterIo | null): void {
	routerIo = io;
}

export function runPadActions(actions: readonly PadAction[]): void {
	if (!padModeActive()) return;
	for (const action of actions) {
		// The controller map is the read-the-buttons surface: exploring the pad
		// must not fire what the diagram documents, and the diagram IS the
		// legend while it is up. Held state is published by the poll itself, so
		// every cap still lights — only the side effects stand down. Start
		// toggles the map shut and B closes it; nothing else gets through.
		const mapOpen = controllerMapOpen();
		if (mapOpen && action !== "controller-map" && action !== "back") continue;
		if (!mapOpen) {
			setHintContext(currentHintContext());
			setHintsVisible(true);
			clearTimeout(hintTimer);
			hintTimer = setTimeout(() => setHintsVisible(false), PAD_HINT_IDLE_MS);
		}
		dispatchPadAction(action);
		diag("pad.action", { action, context: hintContext() });
	}
}

// ── private ──────────────────────────────────────────────────────────────────

interface PadPaneTarget {
	ctx: DeckOpsCtx;
	leaf: PaneLeaf;
	layout: Layout;
}

let routerIo: PadRouterIo | null = null;
let hintTimer: ReturnType<typeof setTimeout> | undefined;
// The key-pad focus retry outlives the press that started it, so a second open
// must cancel the first: two live rAF retries would fight over focus.
let cancelKeypadFocus: (() => void) | null = null;
// A Gamepad press carries no user activation, so a browser that gates
// getUserMedia on a gesture denies a pad-started mic without even prompting.
// Latched so a mashed stick click does not stack copies of the same advice.
let micGestureRequired = false;

function dispatchPadAction(action: PadAction): void {
	switch (action) {
		case "move-up":
		case "move-down": {
			// The focused scroll box owns ↑/↓ until clamped, then the direction
			// becomes focus travel — the scroll write's return value IS the clamp.
			const box = focusedTerminalBox();
			const delta = action === "move-up" ? -PAD_SCROLL_STEP_PX : PAD_SCROLL_STEP_PX;
			if (box && scrollTerminalReaderBox(box, delta)) return;
			padKey(action === "move-up" ? "ArrowUp" : "ArrowDown");
			return;
		}
		case "move-left":
			padKey("ArrowLeft");
			return;
		case "move-right":
			padKey("ArrowRight");
			return;
		case "scroll-up":
		case "scroll-down": {
			const target = targetPane();
			if (!target) return;
			const box = document.querySelector<HTMLElement>(
				`[data-pane-id="${target.leaf.paneId}"] .wterm`,
			);
			// Scroll-only: the right stick never falls through to focus travel.
			if (box)
				scrollTerminalReaderBox(
					box,
					action === "scroll-up" ? -PAD_SCROLL_STEP_PX : PAD_SCROLL_STEP_PX,
				);
			return;
		}
		case "activate": {
			// While a dictation is owned, A is the commit gesture: the pad has no
			// other way to accept a transcript.
			if (voiceDictating()) {
				voiceControls()?.toggle();
				return;
			}
			// On the terminal box the pad has no keyboard, so A opens the one
			// surface that sends raw keys — and lands on a key, or the D-pad
			// would have nothing to travel between.
			if (focusedTerminalBox()) {
				toggleTerminalNavPad();
				if (terminalNavPadOpen()) startKeypadFocus();
				return;
			}
			const active = document.activeElement as HTMLElement | null;
			if (!padKey("Enter")) return;
			// An untrusted key never activates a native button; unconsumed ⏎ means
			// nobody claimed it, so the click is ours to make.
			active?.click();
			return;
		}
		case "back": {
			if (voiceDictating()) {
				voiceControls()?.discard();
				return;
			}
			const active = document.activeElement as HTMLElement | null;
			// The key pad renders in a body portal, so an Escape from inside it
			// would escape past it to whatever owns the document. B leaves the
			// pad explicitly and hands the terminal its focus back.
			if (terminalNavPadOpen() && active?.closest(".term-nav")) {
				closeTerminalNavPad();
				paneTerminalBox()?.focus();
				return;
			}
			// Kobalte dismisses on an untrusted document keydown WITHOUT calling
			// preventDefault, so a dispatched Escape would close the map and let
			// the rest of this chain close the drawer behind it too. One press,
			// one effect: close it here.
			if (controllerMapOpen()) {
				closeControllerMap();
				return;
			}
			if (!padKey("Escape")) return;
			if (active?.matches(".terminal-input")) {
				// The PTY textarea consumes every arrow, so a pad that cannot leave
				// it is stuck there for the pane's life.
				const box = active.closest<HTMLElement>(".wterm");
				active.blur();
				box?.focus();
				return;
			}
			if (uiStore.sidebarOpen) closeSidebar();
			return;
		}
		case "palette":
			if (cmdPaletteOpen()) closeCmdPalette();
			else openCmdPalette();
			return;
		case "context-menu": {
			const active = document.activeElement as HTMLElement | null;
			if (!active) return;
			const rect = active.getBoundingClientRect();
			active.dispatchEvent(
				new MouseEvent("contextmenu", {
					bubbles: true,
					cancelable: true,
					button: 2,
					clientX: rect.left + rect.width / 2,
					clientY: rect.top + rect.height / 2,
				}),
			);
			return;
		}
		case "tab-prev":
			stepTab(-1);
			return;
		case "tab-next":
			stepTab(1);
			return;
		case "pane-prev":
			stepPane(-1);
			return;
		case "pane-next":
			stepPane(1);
			return;
		case "keypad":
			toggleTerminalNavPad();
			if (terminalNavPadOpen()) startKeypadFocus();
			return;
		case "controller-map":
			if (controllerMapOpen()) closeControllerMap();
			else openControllerMap();
			return;
		case "mic-toggle":
			toggleDictation(action);
			return;
		case "folder-next":
			stepFolder();
			return;
	}
}

/** Dispatch one synthetic key on the focused element. false ⇒ a handler
 *  consumed it, which is how this module asks "did anything already own this?" */
function padKey(key: string): boolean {
	const target = (document.activeElement as HTMLElement | null) ?? document.body;
	return target.dispatchEvent(
		new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
	);
}

function focusedTerminalBox(): HTMLElement | null {
	const active = document.activeElement as HTMLElement | null;
	return active?.matches(".wterm") ? active : null;
}

/** Focus the key pad's first key, cancelling a superseded attempt: the helper
 *  retries across frames until the portal paints, so two live retries would
 *  fight over which key the D-pad starts from. */
function startKeypadFocus(): void {
	cancelKeypadFocus?.();
	cancelKeypadFocus = focusTerminalNavPadFirstKey();
}

/** The terminal box a portal-rendered surface hands focus back to: the target
 *  pane's, else the only one painted. */
function paneTerminalBox(): HTMLElement | null {
	const target = targetPane();
	const scoped = target
		? document.querySelector<HTMLElement>(
			`[data-pane-id="${target.leaf.paneId}"] .wterm`,
		)
		: null;
	return scoped ?? document.querySelector<HTMLElement>(".wterm");
}

function toggleDictation(action: PadAction): void {
	const controls = voiceControls();
	// The composer that owns the mic mounts per focused pane, so a dead button
	// here is a real state, not a bug — say which state it was.
	if (!controls) {
		diag("pad.action_unavailable", { action, reason: "no-composer" });
		return;
	}
	// Stopping never needs permission; only a start does, and the pad cannot
	// supply the user activation some browsers demand for it.
	if (!voiceDictating() && !controls.canStartWithoutGesture()) {
		diag("pad.action_unavailable", { action, reason: "mic-needs-gesture" });
		if (!micGestureRequired) {
			micGestureRequired = true;
			addToast(
				"Tap the mic once to allow it — the controller can start it after that.",
				"warn",
			);
		}
		return;
	}
	micGestureRequired = false;
	controls.toggle();
}

function stepFolder(): void {
	const io = routerIo;
	if (!io) return;
	const session = activeSessionForPath(io.getPath());
	const next = nextFolderSessionId(
		buildFolderGroups(),
		session ? folderKeyOf(session) : null,
	);
	if (next) io.navigate(`/s/${next}`);
}

/** The pane the deck actions address: the one holding DOM focus, else the
 *  layout's focused pane. Directional travel deliberately does not move
 *  focusedPaneId, so mere focus movement never fires focusPaneOp's navigation. */
function targetPane(): PadPaneTarget | null {
	const io = routerIo;
	if (!io) return null;
	const session = activeSessionForPath(io.getPath());
	if (!session || session.status !== "open") return null;
	const folderKey = folderKeyOf(session);
	const layout = resolveLayout(folderKey, liveSessionIdsForFolder(folderKey));
	const paneId = (document.activeElement as HTMLElement | null)
		?.closest<HTMLElement>("[data-pane-id]")
		?.getAttribute("data-pane-id")
		?? layout.focusedPaneId;
	const leaf = allLeaves(layout.root).find((candidate) => candidate.paneId === paneId);
	if (!leaf) return null;
	return { ctx: deckOpsCtxForFolder(folderKey, io), leaf, layout };
}

function stepTab(step: number): void {
	const target = targetPane();
	if (!target || target.leaf.tabs.length < 2) return;
	const tabs = target.leaf.tabs;
	const current = tabs.indexOf(target.leaf.selectedTab);
	// Wraparound: a console UI cycles rather than dead-ending at an end stop.
	const next = tabs[(current + step + tabs.length) % tabs.length];
	if (next) selectTabOp(target.ctx, next, spotlitPaneIdIn(target.layout));
}

function stepPane(step: number): void {
	const target = targetPane();
	if (!target) return;
	const leaves = allLeaves(target.layout.root);
	if (leaves.length < 2) return;
	const current = leaves.findIndex((leaf) => leaf.paneId === target.leaf.paneId);
	const next = leaves[(current + step + leaves.length) % leaves.length];
	if (next) focusPaneOp(target.ctx, next.paneId);
}

function currentHintContext(): PadHintContext {
	if (voiceDictating()) return "dictation";
	const active = document.activeElement as HTMLElement | null;
	if (active?.closest(".term-nav")) return "keypad";
	// The map stands the dispatcher down and is its own legend, so the hint
	// state is never refreshed while it is open.
	if (cmdPaletteOpen() || helpOpen()) return "overlay";
	if (active?.closest('[role="menu"],[role="dialog"]')) return "menu";
	if (focusedTerminalBox()) return "terminal";
	return "default";
}
