// Right-click action menu for a sidebar SessionRow: duplicate/restart/
// open-in-finder/transfer/close. Rendered as a fixed-position popover at
// the cursor with a click-away scrim. Owns its own duplicate/restart/
// finder/transfer handlers; "Close terminal" delegates to the row's
// soft-close (props.onDelete) — the SAME action as the row's ✕ — so the
// 5s undo window stays single-sourced in SessionRow. Extracted from
// SessionRow.tsx to keep it under the 400-line cap (CLAUDE.md standards).
//
// Props: session, pos {x,y}, onClose(), onDelete(MouseEvent).

import { Show, For } from "solid-js";
import { Portal } from "solid-js/web";
import { useNavigate } from "@solidjs/router";
import type { Session } from "@roost/shared/wire";
import { rootStore } from "../../store/root.ts";
import { sessionWorkerIsOffline } from "../../store/worker-removal.ts";
import { openRenameDialog } from "../../store/renameDialog.ts";
import { folderHeadline } from "../../lib/sessionTitle.ts";
import { supportedWorkerPlatform } from "../../lib/nativePath.ts";
import { invokeMachineAction, machineActionsForWorker } from "../../lib/machineActions.ts";
import type { MachineActionDefinition } from "../../lib/machineActions.ts";
import { addToast } from "../../store/toastStore.ts";
import { openTransferDialog } from "../../lib/transferDialog.ts";
import {
	ctxMenuSurfaceStyle,
	CtxMenuItem,
	CtxMenuSeparator,
} from "../contextMenuPrimitives.tsx";

interface SessionRowContextMenuProps {
	session: Session;
	pos: { x: number; y: number };
	onClose: () => void;
	onDelete: (e: MouseEvent) => void;
}


export function SessionRowContextMenu(props: SessionRowContextMenuProps) {
	const navigate = useNavigate();
	const session = () => props.session;

	// Pre-fill with the current custom name, or the displayed auto title if none
	// (`customTitle ?? title`). The dialog commits via coordClient.
	function handleRename() {
		props.onClose();
		const s = session();
		openRenameDialog({
			sessionId: s.id,
			currentTitle: s.custom_title ?? folderHeadline(s),
			hasCustom: !!s.custom_title,
		});
	}

	// Manual restart for one specific terminal. The new session gets a fresh
	// session_id; the row briefly disappears and a sibling appears at the same
	// workspace position.
	async function handleRestart() {
		props.onClose();
		const { addToast } = await import("../../store/toastStore.ts");
		try {
			const { coordClient } = await import("../../connect.ts");
			const s = session();
			const res = await coordClient.sessionsSpawn({
				workerFp: s.worker_fp,
				kind: s.kind,
				folder: s.cwd,
			});
			await coordClient.sessionsKill({ sessionId: s.id });
			navigate(`/s/${res.sessionId}`);
		} catch (err) {
			addToast(
				`Restart failed: ${err instanceof Error ? err.message : String(err)}`,
				"err",
			);
		}
	}

	function handleTransfer() {
		props.onClose();
		openTransferDialog();
	}

	// New terminal on the same server, in the same cwd. Unlike Restart it
	// leaves the source session alone — you end up with two terminals on the
	// same folder.
	async function handleDuplicate() {
		props.onClose();
		const { addToast } = await import("../../store/toastStore.ts");
		try {
			const { coordClient } = await import("../../connect.ts");
			const s = session();
			const res = await coordClient.sessionsSpawn({
				workerFp: s.worker_fp,
				kind: s.kind,
				folder: s.cwd,
			});
			navigate(`/s/${res.sessionId}`);
		} catch (err) {
			addToast(
				`Duplicate failed: ${err instanceof Error ? err.message : String(err)}`,
				"err",
			);
		}
	}

	// The owning worker is offline (asleep, dead, or permanently removed) → a
	// normal close cannot be acknowledged. Force remove remains available for
	// sessions intentionally retained after machine offboarding.
	const workerIsOffline = () =>
		sessionWorkerIsOffline(rootStore.workers[session().worker_fp]);

	async function handleForceRemove() {
		props.onClose();
		const { addToast } = await import("../../store/toastStore.ts");
		try {
			const { coordClient } = await import("../../connect.ts");
			const res = await coordClient.sessionsKill({
				sessionId: session().id,
				force: true,
			});
			if (!res.accepted) addToast("Force remove not accepted", "err");
		} catch (err) {
			addToast(
				`Force remove failed: ${err instanceof Error ? err.message : String(err)}`,
				"err",
			);
		}
	}

	// ONLY the live reachable address is actionable; labels are never DNS names.
	const workerHost = (): string | null => {
		const w = rootStore.workers[session().worker_fp];
		return w?.reachable_addr && w.reachable_addr.length > 0 ? w.reachable_addr : null;
	};
	const hasReachableAddr = () => workerHost() !== null;
	const NO_ADDR_TOOLTIP =
		"No reachable address yet — the machine's worker must heartbeat its live tailnet name first.";

	async function handleMachineAction(action: MachineActionDefinition) {
		const host = workerHost();
		props.onClose();
		if (!host) return;
		try {
			await invokeMachineAction(action, host);
			if (action.id === "network-share") addToast("Network share path copied", "ok");
		} catch (err) {
			addToast(`Machine action failed: ${err instanceof Error ? err.message : String(err)}`, "err");
		}
	}

	const machineItems = () => {
		const worker = rootStore.workers[session().worker_fp];
		const platform = supportedWorkerPlatform(worker?.os);
		if (!platform) return [];
		return machineActionsForWorker(platform, true).map((action) => ({
			label: action.label,
			testid: action.id,
			onClick: () => void handleMachineAction(action),
			disabled: !hasReachableAddr(),
			title: hasReachableAddr() ? undefined : NO_ADDR_TOOLTIP,
		}));
	};

	const primaryItems = () => [
		{ label: "Rename…", testid: "rename", onClick: () => handleRename() },
		{
			label: "Duplicate terminal",
			testid: "duplicate",
			onClick: () => void handleDuplicate(),
		},
		{
			label: "Restart",
			testid: "restart",
			onClick: () => void handleRestart(),
		},
		...machineItems(),
		// Cross-worker transfer only when there's another worker to target.
		...(Object.keys(rootStore.workers).length > 1
			? [
					{
						label: "Transfer files (beta)…",
						testid: "transfer",
						onClick: () => void handleTransfer(),
					},
				]
			: []),
	];
	// Destructive actions — rendered below a separator (matches the terminal menu).
	const dangerItems = () => [
		// Same action as the row's ✕ — soft-close with a 5s undo.
		{
			label: "Close terminal",
			testid: "delete",
			onClick: (e: MouseEvent) => {
				props.onClose();
				props.onDelete(e);
			},
		},
		// Only when the worker is offline (a normal close can't be acked).
		...(workerIsOffline()
			? [
					{
						label: "Force remove (worker offline)",
						testid: "force-remove",
						onClick: () => void handleForceRemove(),
					},
				]
			: []),
	];

	return (
		<Show when={props.pos}>
			{/* Portal to <body>: the SessionRow root carries transform:translateX
         (swipe-to-close) + overflow:hidden, which makes this position:fixed
         menu resolve against the row and get clipped to nothing. Same fix as
         TerminalContextMenu. */}
			<Portal>
				{/* Click-away scrim */}
				<div
					style={{ position: "fixed", inset: "0", "z-index": 99 }}
					onClick={(e) => {
						e.stopPropagation();
						props.onClose();
					}}
					onContextMenu={(e) => {
						e.preventDefault();
						props.onClose();
					}}
				/>
				{/* Surface + items come from the SHARED contextMenuPrimitives, so this
          looks identical to the terminal-pane menu (same bg/border/radius/
          shadow/padding/font, same item hover, same separator). z-index 100 to
          clear the click-away scrim (99). */}
				<div
					data-testid={`session-context-menu-${session().id}`}
					class="df-menu-enter"
					style={ctxMenuSurfaceStyle(props.pos.x, props.pos.y, 100)}
					onClick={(e) => e.stopPropagation()}
				>
					<For each={primaryItems()}>
						{(item) => (
							<CtxMenuItem
								testid={`session-ctx-${item.testid}-${session().id}`}
								onClick={item.onClick as (e: MouseEvent) => void}
								disabled={
									"disabled" in item ? (item.disabled as boolean) : undefined
								}
								title={
									"title" in item
										? (item.title as string | undefined)
										: undefined
								}
							>
								{item.label}
							</CtxMenuItem>
						)}
					</For>
					<CtxMenuSeparator />
					<For each={dangerItems()}>
						{(item) => (
							<CtxMenuItem
								testid={`session-ctx-${item.testid}-${session().id}`}
								onClick={item.onClick as (e: MouseEvent) => void}
								danger
							>
								{item.label}
							</CtxMenuItem>
						)}
					</For>
				</div>
			</Portal>
		</Show>
	);
}
