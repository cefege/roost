// Agents sidebar panel for retained coding-agent sessions.
// It projects navigation documents through the existing folder order and status
// presentation, then delegates terminal navigation to the shared list primitives.
// FolderList remains the sole owner of global sidebar cursor behavior.

import { createMemo, For, Show, type JSX } from "solid-js";
import { useLocation } from "@solidjs/router";
import { AgentStatusIndicator } from "../AgentStatusIndicator.tsx";
import { EmptyState, List, ListRow, StatusDot } from "../Settings/md/primitives.tsx";
import { seenAgentRevision } from "../../lib/agentSeen.ts";
import { AGENT_STATUS_PRESENTATION } from "../../lib/agentStatus.ts";
import { buildFolderGroups } from "../../lib/folderGroups.ts";
import { pushRecent } from "../../lib/sidebarRecent.ts";
import { activeSessionForPath } from "../../store/selectors.ts";
import { navigationSearchDocuments } from "../../store/navigation-search.ts";
import { rootStore } from "../../store/root.ts";
import { closeSidebar } from "../../store/uiStore.ts";
import { projectSidebarAgentGroups } from "./sidebarAgentsProjection.ts";

export function SidebarAgents(): JSX.Element {
  const location = useLocation();
  const activeSessionId = createMemo(() => activeSessionForPath(location.pathname)?.id ?? null);
  const groups = createMemo(() => projectSidebarAgentGroups({
    documents: navigationSearchDocuments(),
    folderGroups: buildFolderGroups(),
    sessions: rootStore.sessions,
    agentStatuses: rootStore.agent_status,
    seenRevision: seenAgentRevision,
  }));

  function recordNavigation(event: MouseEvent, sessionId: string): void {
    // Respect a cancelled link click; modified clicks retain native link behavior.
    if (
      event.defaultPrevented
      || event.button !== 0
      || event.metaKey
      || event.ctrlKey
      || event.altKey
      || event.shiftKey
    ) return;
    pushRecent(sessionId);
    closeSidebar();
  }

  return (
    <section class="workbench-sidebar-agents" data-testid="sidebar-agents" aria-label="Agents">
      <Show
        when={groups().length > 0}
        fallback={(
          <EmptyState
            icon="smart_toy"
            title="No active agents"
            supporting="Active coding agents appear here while their terminal sessions remain open."
          />
        )}
      >
        <For each={groups()}>
          {(group) => (
            <section
              class="workbench-sidebar-agents__group"
              data-testid={`sidebar-agent-group-${group.folder.key}`}
              data-folder-key={group.folder.key}
              aria-label={`${group.folder.name} on ${group.folder.server}`}
            >
              <h3 class="workbench-sidebar-agents__group-title">
                <span class="workbench-sidebar-agents__group-name">{group.folder.name}</span>
                <span class="workbench-sidebar-agents__group-server">{group.folder.server}</span>
              </h3>
              <List class="workbench-sidebar-agents__list">
                <For each={group.rows}>
                  {(row) => (
                    <div
                      class="workbench-sidebar-agents__row"
                      onClick={(event) => recordNavigation(event, row.document.sessionId)}
                    >
                      <ListRow
                        leading="terminal"
                        headline={(
                          <span
                            data-testid={`sidebar-agent-title-${row.document.sessionId}`}
                            title={row.document.displayTitle}
                          >
                            {row.document.displayTitle}
                          </span>
                        )}
                        support={(
                          <span class="workbench-sidebar-agents__metadata">
                            <span data-testid={`sidebar-agent-id-${row.document.sessionId}`}>
                              {row.status.agent_id}
                            </span>
                            <span>{AGENT_STATUS_PRESENTATION[row.level].label}</span>
                            <Show when={!row.document.available}>
                              <span data-testid={`sidebar-agent-availability-${row.document.sessionId}`}>
                                Unavailable
                              </span>
                            </Show>
                          </span>
                        )}
                        trailing={(
                          <span class="workbench-sidebar-agents__trailing">
                            <AgentStatusIndicator sessionId={row.document.sessionId} compact />
                            <StatusDot
                              status={row.document.available ? "ok" : "offline"}
                              title={row.document.available ? "Available" : "Machine unavailable"}
                            />
                          </span>
                        )}
                        href={row.document.href}
                        selected={activeSessionId() === row.document.sessionId}
                        testId={`sidebar-agent-row-${row.document.sessionId}`}
                      />
                    </div>
                  )}
                </For>
              </List>
            </section>
          )}
        </For>
      </Show>
    </section>
  );
}
