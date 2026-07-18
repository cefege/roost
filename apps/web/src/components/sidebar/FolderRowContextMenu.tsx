// Right-click menu for a sidebar FOLDER row (a workspace bucket). Two
// actions, both workspace/machine-scoped — NOT the per-session Duplicate/
// Restart/Close/Finder/Transfer set (those stay on SessionRowContextMenu for
// the needs-you strip's real session rows; on a folder bucket they were
// meaningless, Author 2026-07-05):
//   • Rename…      — names the folder's workspace. First rename creates the
//                    workspace (workspacesCreate, attaching the folder's
//                    sessions); later renames update it. folderDisplayName
//                    resolves it by (worker, folder_path), so the name shows on
//                    every device via the workspace sync stream.
//   • Screen sharing — vnc:// to the owning Mac's desktop. location.href, NOT
//                    window.open(_,'_self') — the latter silently no-ops for a
//                    protocol handler (the bug that made every item look dead).
// Portal + click-away scrim + shared ctxMenu primitives = identical surface to
// the session + terminal menus. Props: pos, workerFp, folderPath, displayName,
// sessionIds, onClose.

import { Show, For } from "solid-js";
import { Portal } from "solid-js/web";
import { rootStore } from "../../store/root.ts";
import { workspaceForFolder } from "../../lib/folderKey.ts";
import { openRenameDialog } from "../../lib/renameDialog.ts";
import { ctxMenuSurfaceStyle, CtxMenuItem } from "../contextMenuPrimitives.tsx";

interface FolderRowContextMenuProps {
	pos: { x: number; y: number };
	workerFp: string;
	folderPath: string;
	displayName: string; // current row label — rename pre-fill
	sessionIds: string[]; // attached to the workspace on first create
	onClose: () => void;
}

// ONLY the worker's reachable_addr (its LIVE tailnet DNSName). NEVER synthesize
// from the worker label — the label is the Tailscale HostName (e.g.
// "worker-host") which does NOT resolve; the DNSName ("coord-host") does.
// The old label+suffix fallback shipped dead vnc://worker-* URLs → macOS "Unable
// to resolve". null ⇒ disable the action. Same derivation as SessionRowContextMenu.
function workerHost(workerFp: string): string | null {
	const w = rootStore.workers[workerFp];
	return w?.reachable_addr && w.reachable_addr.length > 0
		? w.reachable_addr
		: null;
}
const NO_ADDR_TOOLTIP =
	"No reachable address yet — the machine's worker must heartbeat its live tailnet name first.";

export function FolderRowContextMenu(props: FolderRowContextMenuProps) {
	// Capture props into locals up front. The menu renders under
	// <Show when={folderCtxMenu()}>, so its props read from that accessor, which
	// turns null on close — reading props AFTER onClose(), or inside the deferred
	// onCommit closure, throws "Cannot read properties of null" (L11
	// no-props-read-after-dispose). Snapshot, then close/defer freely.
	function handleRename() {
		const workerFp = props.workerFp;
		const folderPath = props.folderPath;
		const displayName = props.displayName;
		const sessionIds = props.sessionIds;
		props.onClose();
		const existing = workspaceForFolder(workerFp, folderPath);
		openRenameDialog({
			headline: "Rename workspace",
			currentTitle: existing?.name ?? displayName,
			hasCustom: false,
			onCommit: async (name) => {
				if (!name) return; // workspace name is min-1; empty = no-op
				const { coordClient } = await import("../../connect.ts");
				const ws = workspaceForFolder(workerFp, folderPath);
				if (ws) {
					await coordClient.workspacesUpdate({
						id: ws.id,
						ifVersion: BigInt(ws.version),
						name,
					});
				} else {
					await coordClient.workspacesCreate({
						workerFp,
						name,
						folderPath,
						attachSessionIds: sessionIds,
					});
				}
			},
		});
	}

	function handleScreenShare() {
		const host = workerHost(props.workerFp);
		props.onClose();
		if (!host) return;
		window.location.href = `vnc://${host}`;
	}

	const hasReachableAddr = () => workerHost(props.workerFp) !== null;
	const items = () => [
		{
			label: "Rename…",
			testid: "rename",
			onClick: handleRename,
			disabled: false,
			title: undefined as string | undefined,
		},
		{
			label: "Screen sharing",
			testid: "screen-share",
			onClick: handleScreenShare,
			disabled: !hasReachableAddr(),
			title: hasReachableAddr() ? undefined : NO_ADDR_TOOLTIP,
		},
	];

	return (
		<Show when={props.pos}>
			<Portal>
				{/* Click-away scrim (z-99) — the folder row carries no transform, but
            keep the Portal + scrim for parity with the session/terminal menus. */}
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
				<div
					data-testid="folder-context-menu"
					class="df-menu-enter"
					style={ctxMenuSurfaceStyle(props.pos.x, props.pos.y, 100)}
					onClick={(e) => e.stopPropagation()}
				>
					<For each={items()}>
						{(item) => (
							<CtxMenuItem
								testid={`folder-ctx-${item.testid}`}
								onClick={item.onClick}
								disabled={item.disabled}
								title={item.title}
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
