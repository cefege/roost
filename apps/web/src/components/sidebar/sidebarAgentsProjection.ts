// Pure projection for the Agents sidebar panel.
// SidebarAgents supplies the canonical navigation documents, folders, sessions,
// statuses, and acknowledgement reader; this module only groups and orders rows.
// Folder identity stays worker-inclusive through folderKeyOf and FolderGroup.key.

import type { AgentStatus, Session } from "@roost/shared/wire";
import type { AgentStatusLevel } from "../../lib/agentStatus.ts";
import { AGENT_STATUS_PRESENTATION, deriveAgentStatusLevel } from "../../lib/agentStatus.ts";
import type { FolderGroup } from "../../lib/folderGroups.ts";
import { folderKeyOf } from "../../lib/folderKey.ts";
import type { NavigationSearchDocument } from "../../store/navigation-search.ts";

export type SidebarAgentLevel = Exclude<AgentStatusLevel, "unknown">;

export interface SidebarAgentRow {
  readonly document: NavigationSearchDocument;
  readonly status: AgentStatus;
  readonly level: SidebarAgentLevel;
}

export interface SidebarAgentGroup {
  readonly folder: FolderGroup;
  readonly rows: readonly SidebarAgentRow[];
}

export interface SidebarAgentProjectionInput {
  readonly documents: readonly NavigationSearchDocument[];
  readonly folderGroups: readonly FolderGroup[];
  readonly sessions: Readonly<Record<string, Session>>;
  readonly agentStatuses: Readonly<Record<string, AgentStatus>>;
  readonly seenRevision: (status: AgentStatus) => number;
}

export function projectSidebarAgentGroups(
  input: SidebarAgentProjectionInput,
): readonly SidebarAgentGroup[] {
  const rowsByFolderKey = new Map<string, SidebarAgentRow[]>();

  for (const document of input.documents) {
    const session = input.sessions[document.sessionId];
    if (!session || session.kind !== "shell" || session.status !== "open") continue;

    const status = input.agentStatuses[session.id];
    if (!status) continue;

    const level = deriveAgentStatusLevel(status, input.seenRevision(status));
    if (level === "unknown") continue;

    const folderKey = folderKeyOf(session);
    const rows = rowsByFolderKey.get(folderKey) ?? [];
    rows.push({ document, status, level });
    rowsByFolderKey.set(folderKey, rows);
  }

  return input.folderGroups.flatMap((folder) => {
    const folderSessionIds = new Set(folder.sessionIds);
    const rows = (rowsByFolderKey.get(folder.key) ?? [])
      .filter((row) => folderSessionIds.has(row.document.sessionId))
      .sort(compareSidebarAgentRows);

    return rows.length > 0 ? [{ folder, rows }] : [];
  });
}

function compareSidebarAgentRows(left: SidebarAgentRow, right: SidebarAgentRow): number {
  return AGENT_STATUS_PRESENTATION[right.level].priority
    - AGENT_STATUS_PRESENTATION[left.level].priority
    || left.document.sessionId.localeCompare(right.document.sessionId);
}
