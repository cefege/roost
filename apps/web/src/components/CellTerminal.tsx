// Composes one canonical cell-grid terminal pane from pane-local controllers.
// The worker owns VT parsing; renderer paints cells and input sends attributed
// bytes through the generation-aware Sync view. Warm panes remain mounted while
// the shared viewActive accessor gates publication, focus, and global listeners.

import {
	createMemo,
	createSignal,
	getOwner,
	onCleanup,
	onMount,
	runWithOwner,
	Show,
} from "solid-js";
import { useNavigate } from "@solidjs/router";
import type { MouseTracking } from "@roost/shared/cell";
import { signal } from "@roost/shared/diag";
import { getSessionTraceId } from "../lib/diag.ts";
import { isCompact, isTouchDevice } from "../lib/windowSizeClass.ts";
import { isPendingSpawn } from "../store/optimisticSpawn.ts";
import { createTerminalView } from "../store/terminal-stream.ts";
import { uiStore } from "../store/uiStore.ts";
import { TerminalComposeButton } from "./TerminalComposeButton.tsx";
import { TerminalContextMenu } from "./TerminalContextMenu.tsx";
import { TerminalFindBar } from "./TerminalFindBar.tsx";
import { TerminalNavButtons } from "./TerminalNavButtons.tsx";
import {
	TerminalLoadingNotice,
	TerminalOfflineNotice,
} from "./TerminalOfflineNotice.tsx";
import { Button, Dialog } from "./Settings/md/primitives.tsx";
import {
	mountCellTerminalInteractions,
	type CellTerminalInteractions,
} from "./cell-terminal-interactions.ts";
import {
	mountCellTerminalLifecycle,
	type CellTerminalLifecycle,
} from "./cell-terminal-lifecycle.ts";
import { createCellTerminalInput } from "./cell-terminal-input.ts";
import { createCellTerminalPresentation } from "./cell-terminal-presentation.ts";
import {
	mountCellTerminalRenderer,
	type CellTerminalRendererMount,
} from "./cell-terminal-renderer.ts";
import { createCellTerminalRuntime } from "./cell-terminal-runtime.ts";
import type { CellTerminalProps } from "./cell-terminal-types.ts";
import { createCellTerminalViewport } from "./cell-terminal-viewport.ts";

export function CellTerminal(props: CellTerminalProps) {
	const sessionId = props.session.id;
	const navigate = useNavigate();
	let displayRef: HTMLDivElement | undefined;
	const runtime = createCellTerminalRuntime(sessionId, () => displayRef);
	const [altScreen, setAltScreen] = createSignal(false);
	const [mouseTracking, setMouseTracking] = createSignal<MouseTracking>(0);
	const pending = createMemo(() => isPendingSpawn(sessionId));
	const viewActive = createMemo(
		() => props.inLayout === true
			&& props.surfaceVisible
			&& props.surfaceActive,
	);
	const input = createCellTerminalInput(props, runtime);
	const presentation = createCellTerminalPresentation(
		props,
		runtime,
		pending,
		viewActive,
		navigate,
	);
	const viewport = createCellTerminalViewport(
		runtime,
		presentation,
		pending,
		viewActive,
	);
	const cellOwner = getOwner();
	let rendererMount: CellTerminalRendererMount | null = null;
	let interactionsMount: CellTerminalInteractions | null = null;
	let lifecycleMount: CellTerminalLifecycle | null = null;
	let releaseViewStatus: (() => void) | null = null;
	let releaseViewProgress: (() => void) | null = null;

	onMount(() => {
		const view = createTerminalView(sessionId);
		runtime.view = view;
		releaseViewStatus = view.subscribeStatus((status) => {
			presentation.setViewStatus(status);
			presentation.setViewportLiveReady(
				status.status === "accepted"
					&& status.active
					&& status.baselineReady,
			);
		});
		releaseViewProgress = view.subscribeProgress(presentation.setAttachProgress);
		try {
			runWithOwner(cellOwner, () => {
				rendererMount = mountCellTerminalRenderer(
					props,
					runtime,
					input,
					presentation,
					viewport,
					{ pending, setAltScreen, setMouseTracking },
				);
				interactionsMount = mountCellTerminalInteractions(
					props,
					runtime,
					input,
					presentation,
					viewport,
					pending,
					navigate,
					{ mouseTracking },
				);
				lifecycleMount = mountCellTerminalLifecycle(
					props,
					runtime,
					input,
					presentation,
					viewport,
					pending,
				);
			});
		} catch (error) {
			rendererMount?.disconnectStream();
			lifecycleMount?.dispose();
			interactionsMount?.dispose();
			viewport.parkView();
			rendererMount?.disposeResources();
			runtime.inputController?.destroy();
			runtime.inputController = null;
			releaseViewProgress?.();
			releaseViewProgress = null;
			releaseViewStatus?.();
			releaseViewStatus = null;
			view.dispose();
			runtime.view = null;
			signal("diag.corruption_signal", {
				kind: "cell_mount_failed",
				sid: sessionId,
				session_trace_id: getSessionTraceId(sessionId),
				msg: String(error),
				cooldownKey: sessionId,
			});
		}
	});

	onCleanup(() => {
		runtime.unmounted = true;
		rendererMount?.disconnectStream();
		lifecycleMount?.dispose();
		interactionsMount?.dispose();
		viewport.parkView();
		rendererMount?.disposeResources();
		presentation.dispose();
		input.dispose();
		releaseViewProgress?.();
		releaseViewStatus?.();
		runtime.view?.dispose();
		runtime.view = null;
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
			<Show when={presentation.presentationState() === "receiving"}>
				<div
					class="terminal-stream-indicator"
					data-testid="terminal-stream-indicator"
					data-state="receiving"
					title="Receiving terminal frames"
					aria-hidden="true"
				/>
			</Show>
			<Show when={presentation.presentationState() === "catching_up"}>
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
			<Show when={input.find.open()}>
				<TerminalFindBar
					find={input.find}
					altScreen={altScreen()}
					onDismiss={() => {
						input.find.closeFind();
						requestAnimationFrame(() => viewport.publishViewportNow());
						runtime.inputController?.forceFocus();
					}}
				/>
			</Show>
			<div
				ref={displayRef}
				data-testid="terminal-display"
				style={{
					flex: "1",
					"min-width": "0",
					"min-height": "0",
					"touch-action": "pan-y",
				}}
			/>
			{/* Unbracketed multi-line paste confirmation. Registered in FOCUS_OWNERS
          (md-dialog) so the pane's focus guards let its buttons take focus.
          Mounted ONLY while a paste is pending: md-dialog keeps its slotted
          content in the DOM even when closed, and this subtree sits inside the
          pane, so leaving it mounted would fold dialog prose into the pane's
          textContent — which the smoke harness reads as terminal output. */}
			<Show when={input.pendingPaste() !== null}>
				<Dialog
					open
					onClose={() => input.setPendingPaste(null)}
					headline="Paste multiple lines?"
					actions={
						<>
							<Button
								variant="text"
								data-testid="paste-guard-cancel"
								onClick={() => input.setPendingPaste(null)}
							>
								Cancel
							</Button>
							<Button
								variant="filled"
								data-testid="paste-guard-send"
								onClick={() => {
									const text = input.pendingPaste();
									input.setPendingPaste(null);
									if (text !== null) input.sendTerminalText(text);
								}}
							>
								Paste {input.pendingPasteLines()} lines
							</Button>
						</>
					}
				>
					<p class="md-body-m" style={{ margin: "0" }}>
						This shell has bracketed paste off, so all {input.pendingPasteLines()} lines run
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
					onKey={(key: string) => { runtime.inputController?.dispatchKeydown(key); }}
					ctrlArmed={input.ctrlArmed()}
					onCtrlArmedChange={(armed: boolean) => {
						if (armed && !isTouchDevice()) runtime.inputController?.forceFocus();
						input.setCtrlArmed(armed);
					}}
				/>
				<TerminalComposeButton
					placement="viewport"
					session={props.session}
					active={props.surfaceActive}
					onSubmit={(text) => input.sendTerminalText(text, true)}
					onAttachFiles={input.attachSelectedFiles}
					readContext={input.readTerminalContext}
					captureTerminalSelection={presentation.captureTerminalSelection}
				/>
			</Show>
			{/* Parked desktop composers stay mounted to preserve display height. */}
			<Show when={!isCompact()}>
				<TerminalComposeButton
					placement="pane"
					session={props.session}
					active={props.inLayout === true && props.surfaceActive}
					onSubmit={(text) => input.sendTerminalText(text, true)}
					onAttachFiles={input.attachSelectedFiles}
					readContext={input.readTerminalContext}
					captureTerminalSelection={presentation.captureTerminalSelection}
				/>
			</Show>
			<TerminalContextMenu
				session={props.session}
				getContainer={() => displayRef ?? null}
				onAttachFile={input.attachSelectedFiles}
				onPasteText={input.pasteText}
				onOpenLink={(anchor) => { runtime.linkAttachment?.openLink(anchor); }}
				describeLink={(anchor) =>
					runtime.linkAttachment?.describeLink(anchor) ?? null}
			/>
			<Show when={presentation.loadingNotice()}>
				{(notice) => (
					<TerminalLoadingNotice
						{...notice()}
						progress={presentation.loadingProgress()}
						stuckReason={presentation.stuckReason()}
					/>
				)}
			</Show>
			<Show when={presentation.offline()}>
				<TerminalOfflineNotice
					onRetry={presentation.retryOffline}
					onOpenSibling={presentation.openOfflineSibling}
					hasSibling={!!presentation.offlineSibling()}
				/>
			</Show>
		</div>
	);
}
