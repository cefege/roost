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
import { workerOnline } from "../../store/sync.ts";
import { openRenameDialog } from "../../lib/renameDialog.ts";
import { folderHeadline } from "../../lib/sessionTitle.ts";
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
		const { addToast } = await import("../../lib/toastStore.ts");
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

	async function handleTransfer() {
		props.onClose();
		const { openTransferDialog } = await import("../../lib/transferDialog.ts");
		const srcLabel =
			rootStore.workers[session().worker_fp]?.label ??
			session().worker_fp.slice(0, 8);
		openTransferDialog({
			srcFp: session().worker_fp,
			srcLabel,
			srcPath: session().cwd,
		});
	}

	// New terminal on the same server, in the same cwd. Unlike Restart it
	// leaves the source session alone — you end up with two terminals on the
	// same folder.
	async function handleDuplicate() {
		props.onClose();
		const { addToast } = await import("../../lib/toastStore.ts");
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

	// The owning worker is offline (asleep/dead) → a normal close can't be acked,
	// so the row is stuck. Force remove tombstones it in coord; if the worker ever
	// returns with the PTY still live, coord reaps it on snapshot.
	const workerIsOffline = () => {
		const w = rootStore.workers[session().worker_fp];
		return !!w && !workerOnline(w);
	};

	async function handleForceRemove() {
		props.onClose();
		const { addToast } = await import("../../lib/toastStore.ts");
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

	// ONLY the worker's reachable_addr (its LIVE tailnet DNSName, re-resolved each
	// heartbeat). NEVER synthesize from the worker label — the label is the
	// Tailscale HostName (e.g. "worker-host"), which is NOT a resolvable DNS
	// name; the DNSName ("coord-host") is. null ⇒ the action is disabled, not
	// pointed at a corpse. Shared by Finder (smb://) + Screen Sharing (vnc://).
	const workerHost = (): string | null => {
		const w = rootStore.workers[session().worker_fp];
		return w?.reachable_addr && w.reachable_addr.length > 0
			? w.reachable_addr
			: null;
	};
	const hasReachableAddr = () => workerHost() !== null;
	const NO_ADDR_TOOLTIP =
		"No reachable address yet — the machine's worker must heartbeat its live tailnet name first.";

	function handleOpenInFinder() {
		const host = workerHost();
		props.onClose();
		if (!host) return;
		// location.href, NOT window.open(_,'_self') — the latter silently no-ops for
		// a non-http protocol handler; this actually hands smb:// to macOS.
		window.location.href = `smb://${host}`;
	}

	// Screen Sharing (macOS VNC) to the owning Mac's DESKTOP. The only way to
	// click acceptance dialogs (permission prompts, GUI confirms) on a remote
	// Mac you're not physically at — without it, a worker that pops a dialog
	// deadlocks the whole flow. vnc://<host> opens Screen Sharing.app; over
	// tailnet the FQDN resolves. Restored from commit 8a973dbf (lost in a
	// sidebar redesign).
	function handleScreenShare() {
		const host = workerHost();
		props.onClose();
		if (!host) return;
		// location.href, NOT window.open(_,'_self') — see handleOpenInFinder.
		window.location.href = `vnc://${host}`;
	}

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
		{
			label: "Open in Finder",
			testid: "finder",
			onClick: () => handleOpenInFinder(),
			disabled: !hasReachableAddr(),
			title: hasReachableAddr() ? undefined : NO_ADDR_TOOLTIP,
		},
		{
			label: "Screen sharing",
			testid: "screen-share",
			onClick: () => handleScreenShare(),
			disabled: !hasReachableAddr(),
			title: hasReachableAddr() ? undefined : NO_ADDR_TOOLTIP,
		},
		// Cross-worker transfer only when there's another worker to target.
		...(Object.keys(rootStore.workers).length > 1
			? [
					{
						label: "Transfer files to…",
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
