// Native-selection guard for a cell terminal pane. The renderer replaces DOM on
// every accepted frame, so a selection the user is still holding — or one the
// composer must briefly yield to a focused textarea — has to be captured by ROW
// IDENTITY and revalidated before it is ever restored. A canonical repair may
// retain the same text while replacing its nodes; resurrecting that detached
// range is the bug this module exists to make impossible.
//
// Selection-API call ordering here is load-bearing: Chromium resets the native
// editing target only through the Selection-wide clear, and it can dispatch a
// reveal scroll after animation callbacks. Nothing in here may be reordered.
//
// The pane owns the lifetimes this reads (display element, renderer, backfill
// controller, link attachment) and threads them in as accessors, so the guard
// never holds a stale reference across a remount.

import type {
	CellGridRenderer,
	LiveInteractionResult,
} from "./cellRenderer.ts";
import type { ScrollbackBackfill } from "./scrollbackBackfill.ts";
import type { TerminalLinkAttachment } from "../components/terminal-links.ts";
import type { TerminalSelectionGuard } from "../components/TerminalComposeButton.tsx";

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

export interface TerminalSelectionGuardDeps {
	getDisplay: () => HTMLDivElement | undefined;
	getRenderer: () => CellGridRenderer | null;
	getBackfill: () => ScrollbackBackfill | null;
	getLinkAttachment: () => TerminalLinkAttachment | null;
}

export interface TerminalSelectionGuardController {
	/** A renderer transition that moved the bottom anchor invalidates the paged
	 *  history window the backfill controller is holding. */
	notifyBackfill(result: LiveInteractionResult | undefined): void;
	/** Recompute the renderer's selection hold from the live native selection. */
	syncNativeSelectionHold(): void;
	/** Retain the current pane selection for a guarded suspend/restore cycle. */
	captureTerminalSelection(): TerminalSelectionGuard | undefined;
	/** Transition to live: drop reader holds and any pane-owned selection. */
	prepareLiveInteraction(): void;
	/** Leaving the visible surface — end every reader interval this pane owns. */
	releasePaintHolds(): void;
}

export function createTerminalSelectionGuard(
	deps: TerminalSelectionGuardDeps,
): TerminalSelectionGuardController {
	const { getDisplay, getRenderer, getBackfill, getLinkAttachment } = deps;
	let selectionGuardEpoch = 0;
	let activeSelectionGuard: TerminalSelectionGuard | null = null;
	let selectionGuardSuspended = false;
	const paneOwnsSelectionEndpoint = (selection: Selection): boolean =>
		(!!selection.anchorNode && !!getDisplay()?.contains(selection.anchorNode))
		|| (!!selection.focusNode && !!getDisplay()?.contains(selection.focusNode));
	const notifyBackfill = (result: LiveInteractionResult | undefined): void => {
		if (result?.anchorChanged) getBackfill()?.onFullFrame();
	};
	const syncNativeSelectionHold = (): void => {
		const selection = getDisplay()?.ownerDocument.getSelection();
		const held =
			selectionGuardSuspended
			|| (
				!!selection
				&& !selection.isCollapsed
				&& selection.rangeCount > 0
				&& paneOwnsSelectionEndpoint(selection)
			);
		if (held) getRenderer()?.enterReading("selection");
		notifyBackfill(getRenderer()?.setSelectionHold(held));
	};
	const discardActiveSelectionGuardForTransition = (): void => {
		const guard = activeSelectionGuard;
		activeSelectionGuard = null;
		selectionGuardSuspended = false;
		guard?.release();
	};
	const captureTerminalSelection = (): TerminalSelectionGuard | undefined => {
		const display = getDisplay();
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
				|| getDisplay() !== saved.display
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
		syncNativeSelectionHold();
		return guard;
	};
	const prepareLiveInteraction = (): void => {
		selectionGuardEpoch += 1;
		discardActiveSelectionGuardForTransition();
		const selection = getDisplay()?.ownerDocument.getSelection();
		const ownedSelection = !!selection && paneOwnsSelectionEndpoint(selection);
		// Renderer state moves first: intent, both composed holds, canonical frame,
		// and bottom anchor change as one transition before DOM callbacks can fire.
		const result = getRenderer()?.prepareLiveInteraction();
		// Ownership was captured before reconciliation could detach the old row.
		// Chromium can dispatch the reveal scroll after animation callbacks. Keep
		// the bracket until that scroll arrives; every later admitted input or
		// explicit reader interaction clears it before changing scroll geometry.
		if (ownedSelection) {
			getRenderer()?.beginLiveSelectionRelease();
			selection?.removeAllRanges();
		}
		getLinkAttachment()?.releaseInteraction();
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
		const selection = getDisplay()?.ownerDocument.getSelection();
		const ownedSelection = !!selection && paneOwnsSelectionEndpoint(selection);
		const result = getRenderer()?.prepareLiveInteraction();
		if (ownedSelection) {
			getRenderer()?.beginLiveSelectionRelease();
			selection?.removeAllRanges();
		}
		getLinkAttachment()?.releaseInteraction();
		notifyBackfill(result);
	};

	return {
		notifyBackfill,
		syncNativeSelectionHold,
		captureTerminalSelection,
		prepareLiveInteraction,
		releasePaintHolds,
	};
}
