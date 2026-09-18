// Where the controller's next-folder press lands. A pad has no pointer to click
// a sidebar folder row, so "switch folders" has to resolve to one session id.
// Pure over a FolderGroup list — no store, no DOM — so the cycling contract has
// exactly one implementation and is unit-testable.
// Callers: lib/padActions.ts. Depends on: lib/folderGroups (type only).

import type { FolderGroup } from "./folderGroups.ts";

/** The session to open for the folder AFTER `currentFolderKey` in `groups`
 *  (already ordered by latest activity), cycling at the end. Each folder lands
 *  on its own most-recently-active session — `leadId`, the same target a
 *  sidebar folder click uses. null when there is nowhere to go. */
export function nextFolderSessionId(
	groups: readonly FolderGroup[],
	currentFolderKey: string | null,
): string | null {
	// One folder is not a cycle, whatever the pad is currently showing: the
	// button exists to move BETWEEN folders, and re-navigating to the session
	// already open would rebuild the pane deck for no visible change.
	if (groups.length < 2) return null;
	const current = currentFolderKey === null
		? -1
		: groups.findIndex((group) => group.key === currentFolderKey);
	// An unknown current folder — a non-session route, or a folder whose
	// sessions have all closed — still has somewhere to go: the newest one.
	if (current < 0) return groups[0].leadId;
	return groups[(current + 1) % groups.length].leadId;
}
