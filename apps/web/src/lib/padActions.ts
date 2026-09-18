// Controller intents → the app's EXISTING focus/menu/deck machinery. Directional
// and activate/back intents become one untrusted synthetic key on the focused
// element, so every roving surface that already owns arrows/Enter/Escape keeps
// owning them; only the two things an untrusted key cannot do (native click
// activation, native scrolling) are compensated explicitly here.
// Callers: App.tsx (installGamepadSource(runPadActions)), UiBridge (router io).
// Depends on: lib/deckOps, lib/keyboardShortcuts, lib/terminalReaderScroll,
// lib/padBindings, lib/padMode, store/paneLayout*, store/uiStore.

import { createSignal } from "solid-js";
import { diag } from "@roost/shared/diag";
import { allLeaves, type Layout, type PaneLeaf } from "../store/paneLayout.ts";
import { resolveLayout } from "../store/paneLayoutStore.ts";
import { activeSessionForPath, liveSessionIdsForFolder } from "../store/selectors.ts";
import { closeSidebar, uiStore } from "../store/uiStore.ts";
import { toggleTerminalNavPad } from "../components/TerminalNavButtons.tsx";
import {
	deckOpsCtxForFolder, focusPaneOp, selectTabOp, spotlitPaneIdIn,
	type DeckOpsCtx,
} from "./deckOps.ts";
import { folderKeyOf } from "./folderKey.ts";
import {
	closeCmdPalette, closeHelp, cmdPaletteOpen, helpOpen, openCmdPalette, openHelp,
} from "./keyboardShortcuts.ts";
import type { PadAction, PadHintContext } from "./padBindings.ts";
import { padModeActive } from "./padMode.ts";
import { PAD_SCROLL_STEP_PX, scrollTerminalReaderBox } from "./terminalReaderScroll.ts";

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
		setHintContext(currentHintContext());
		setHintsVisible(true);
		if (hintTimer !== null) clearTimeout(hintTimer);
		hintTimer = setTimeout(() => setHintsVisible(false), PAD_HINT_IDLE_MS);
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
let hintTimer: ReturnType<typeof setTimeout> | null = null;

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
			// On the terminal box the pad has no keyboard, so A opens the one
			// surface that sends raw keys instead of activating a control.
			if (focusedTerminalBox()) {
				toggleTerminalNavPad();
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
			const active = document.activeElement as HTMLElement | null;
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
			return;
		case "help":
			if (helpOpen()) closeHelp();
			else openHelp();
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
	if (cmdPaletteOpen() || helpOpen()) return "overlay";
	const active = document.activeElement as HTMLElement | null;
	if (active?.closest('[role="menu"],[role="dialog"]')) return "menu";
	if (focusedTerminalBox()) return "terminal";
	return "default";
}
