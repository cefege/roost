// Folder list — sidebar body for non-chat workspace groups.
// Groups use the same worker-and-folder identity as the terminal deck and sort
// by recent terminal activity.
// Owns the sidebar keyboard surface: cursor order and ⏎ activation.
// Reads the session store; no writes.

import { createComputed, createEffect, createMemo, createSignal, For, Show, onMount, onCleanup } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { A, useNavigate, useLocation } from "@solidjs/router";
import { rootStore } from "../../store/root.ts";
import { closeSidebar } from "../../store/uiStore.ts";
import { activeSessionForPath } from "../../store/selectors.ts";
import { getLastSessionForFolder } from "../../lib/lastVisited.ts";
import { cursorSessionId, setActivateHandler, setOrderedSessionIds } from "../../lib/sidebarCursor.ts";
import { relTimeSince } from "../../lib/relTime.ts";
import { folderKeyOf } from "../../lib/folderKey.ts";
import { colorForFp } from "../../lib/fpColor.ts";
import { buildFolderGroups, type FolderGroup, PR_CHECK_GLYPH, PR_CHECK_COLOR } from "../../lib/folderGroups.ts";
import { pushRecent } from "../../lib/sidebarRecent.ts";
import { isChatFolder } from "../../lib/quickChat.ts";
import { relTimeTickMs } from "./SessionRow.tsx";
import { FolderRowContextMenu } from "./FolderRowContextMenu.tsx";
import { FlatNewTerminal } from "./FlatNewTerminal.tsx";
import { FolderGlyph } from "../FolderGlyph.tsx";
import { MachineIdentityMark } from "../MachineIdentityMark.tsx";
import { IconButton } from "../Settings/md/IconButton.tsx";
import { StatusDot } from "../Settings/md/StatusDot.tsx";
import {
  AGENT_STATUS_PRESENTATION,
  formatAgentStatusCounts,
} from "../../lib/agentStatus.ts";
import "@material/web/ripple/ripple.js";

function FolderStatusRollup(props: { group: FolderGroup }) {
  return (
    <Show when={props.group.agentStatus.total > 0 && props.group.agentStatus.level !== "unknown"}>
      <span
        class="agent-status-rollup"
        data-level={props.group.agentStatus.level}
        data-testid={`folder-agent-status-${props.group.key}`}
      >
        <StatusDot status={AGENT_STATUS_PRESENTATION[props.group.agentStatus.level].dotStatus} />
        <span>{formatAgentStatusCounts(props.group.agentStatus.counts)}</span>
      </span>
    </Show>
  );
}
export function FolderList() {
  const navigate = useNavigate();
  const location = useLocation();

  // Right-click menu on a folder row — a workspace/machine-scoped menu (Rename
  // the folder's workspace + Screen sharing to the owning Mac). NOT the
  // per-session menu: Duplicate/Restart/Close/Finder/Transfer are meaningless on
  // a folder bucket (Author 2026-07-05). Payload carries the folder identity the
  // menu needs; sessionIds seed the workspace on the first rename.
  const [folderCtxMenu, setFolderCtxMenu] = createSignal<
    { x: number; y: number; workerFp: string; folderPath: string; displayName: string; sessionIds: string[] } | null
  >(null);

  const activeSession = createMemo(() => activeSessionForPath(location.pathname));

  const activeFolderKey = createMemo(() => {
    const s = activeSession();
    return s ? folderKeyOf(s) : null;
  });

  function targetIdFor(g: FolderGroup): string {
    const rememberedId = getLastSessionForFolder(g.spawnFp, g.spawnCwd);
    const remembered = rememberedId ? rootStore.sessions[rememberedId] : null;
    if (remembered && remembered.status === "open" && folderKeyOf(remembered) === g.key) {
      return remembered.id;
    }
    return g.leadId;
  }


  // Cursor ⏎ opens a session: same effect as a row click (push MRU + nav).
  onMount(() => setActivateHandler((id) => { pushRecent(id); navigate(`/s/${id}`); }));
  onCleanup(() => setActivateHandler(null));


  // ── Folder rows ────────────────────────────────────────────────────
  // Keyed store instead of a memo: `<For>` keys by reference, and the old
  // memo allocated fresh FolderGroup objects per run — every activity tick
  // tore down and recreated EVERY row's DOM. reconcile keyed by `key` keeps
  // object identity for unchanged rows (DOM survives); changed fields flow as
  // fine-grained store updates because the JSX reads g.* off store proxies.
  // FolderGroup is plain scalars only (leadId / sessionIds, not live Session
  // proxies) so the deep diff stays cheap.
  const [gs, setGs] = createStore<{ rows: FolderGroup[] }>({ rows: [] });
  createComputed(() => setGs("rows", reconcile(buildFolderGroups(), { key: "key" })));

  const folderRows = createMemo(() => gs.rows.filter((group) => !isChatFolder(group.spawnCwd)));

  // Cursor order follows the folders shown in this single Spaces projection.
  createEffect(() => {
    setOrderedSessionIds(folderRows().map((group) => group.leadId));
  });

  // Render each non-chat folder with the established row chrome.
  const renderFolderRow = (g: FolderGroup) => {
    // Reopen the tab last visited in this folder (if still open), falling back
    // to the folder's most-recent terminal.
    const targetId = () => targetIdFor(g);
    return (
    <A
      href={`/s/${targetId()}`}
      class="df-row"
      // Row-level title = full cwd. In the collapsed icon rail the
      // body text is hidden, so hovering the glyph must still reveal
      // which working directory it is.
      title={g.spawnCwd}
      data-density="flat"
      data-testid={`folder-row-${g.key}`}
      data-selected={activeFolderKey() === g.key ? "focused" : ""}
      data-cursor={cursorSessionId() === g.leadId ? "on" : undefined}
      onClick={() => { pushRecent(targetId()); closeSidebar(); }}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        setFolderCtxMenu({
          x: e.clientX, y: e.clientY,
          workerFp: g.spawnFp, folderPath: g.spawnCwd,
          displayName: g.name, sessionIds: [...g.sessionIds],
        });
      }}
      style={{ "--avatar-bg": `hsl(${colorForFp(g.key).hue} 48% 42%)` }}
    >
      <md-ripple />
      <span class="df-leading df-leading--machine">
        <MachineIdentityMark worker={rootStore.workers[g.spawnFp]} contextTitle={g.spawnCwd} />
      </span>
      <span class="df-flat-body">
        <span class="df-flat-top">
          <span class="df-label df-flat-headline" title={g.spawnCwd}>{g.name}</span>
          <span class="df-flat-time">{(relTimeTickMs(), relTimeSince(g.latestActivity))}</span>
        </span>
        <Show when={g.subtitle}>
          <span class="df-flat-subtitle">{g.subtitle}</span>
        </Show>
        <FolderStatusRollup group={g} />
        <span class="df-flat-supporting">
          <span class="df-flat-server" data-online={g.online ? "true" : "false"}>
            <svg class="df-flat-server-icon" width="12" height="12" viewBox="0 0 24 24" fill="none"
              stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M20 16V7a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v9m16 0H4m16 0 1.28 2.55a1 1 0 0 1-.9 1.45H3.62a1 1 0 0 1-.9-1.45L4 16" />
            </svg>
            <span class="df-flat-server-text">{g.server}</span>
          </span>
          <span class="df-flat-path" title={`${g.sessionIds.length} pane${g.sessionIds.length === 1 ? "" : "s"} in this workspace`}>
            <FolderGlyph size={11} class="df-flat-folder-icon" />
            <span>{g.sessionIds.length}</span>
          </span>
          <Show when={g.branch}>
            {(branch) => (
              <span class="df-flat-branch" title={`On branch ${branch()}`}>
                <svg class="df-flat-branch-icon" width="11" height="11" viewBox="0 0 24 24" fill="none"
                  stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                  <line x1="6" y1="3" x2="6" y2="15" /><circle cx="18" cy="6" r="3" />
                  <circle cx="6" cy="18" r="3" /><path d="M18 9a9 9 0 0 1-9 9" />
                </svg>
                <span class="df-flat-branch-text">{branch()}</span>
              </span>
            )}
          </Show>
          <Show when={g.pr}>
            {(pr) => (
              <a
                class="df-flat-pr"
                data-testid={`pr-badge-${g.key}`}
                data-pr-state={pr().state}
                data-pr-checks={pr().checks}
                href={pr().url}
                target="_blank"
                rel="noopener noreferrer"
                title={`PR #${pr().number} · ${pr().state} · checks ${pr().checks}`}
                onClick={(e) => { e.stopPropagation(); if (!pr().url) e.preventDefault(); }}
              >
                <svg class="df-flat-pr-icon" width="11" height="11" viewBox="0 0 24 24" fill="none"
                  stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                  <circle cx="6" cy="6" r="3" /><circle cx="6" cy="18" r="3" />
                  <path d="M6 9v6" /><circle cx="18" cy="18" r="3" /><path d="M18 15V9a3 3 0 0 0-3-3h-3" />
                </svg>
                <span class="df-flat-pr-num">#{pr().number}</span>
                <Show when={PR_CHECK_GLYPH[pr().checks]}>
                  <span class="df-flat-pr-check" style={{ color: PR_CHECK_COLOR[pr().checks] }}>
                    {PR_CHECK_GLYPH[pr().checks]}
                  </span>
                </Show>
              </a>
            )}
          </Show>
          <For each={g.ports}>
            {(port) => (
              <a
                class="df-flat-port"
                data-testid={`port-chip-${g.key}-${port}`}
                href={g.reachAddr ? `http://${g.reachAddr}:${port}` : undefined}
                target="_blank"
                rel="noopener noreferrer"
                title={g.reachAddr ? `Open http://${g.reachAddr}:${port}` : `Listening on :${port}`}
                onClick={(e) => { e.stopPropagation(); if (!g.reachAddr) e.preventDefault(); }}
              >:{port}</a>
            )}
          </For>
        </span>
      </span>
      <IconButton
        icon="more_vert"
        label="Folder actions"
        class="df-action"
        data-testid={`folder-more-${g.key}`}
        title="Folder actions"
        style={{ "--md-icon-button-icon-size": "14px" }}
        onClick={(e) => {
          e.stopPropagation();
          e.preventDefault();
          const r = e.currentTarget.getBoundingClientRect();
          setFolderCtxMenu({
            x: r.right, y: r.bottom,
            workerFp: g.spawnFp, folderPath: g.spawnCwd,
            displayName: g.name, sessionIds: [...g.sessionIds],
          });
        }}
      />
    </A>
    );
  };



  return (
    <div data-testid="folder-list">
      <div class="df-flat-group">
        <For each={folderRows()}>
          {(group) => renderFolderRow(group)}
        </For>
      </div>
      <FlatNewTerminal />
      <Show when={folderCtxMenu()}>
        {(m) => (
          <FolderRowContextMenu
            pos={{ x: m().x, y: m().y }}
            workerFp={m().workerFp}
            folderPath={m().folderPath}
            displayName={m().displayName}
            sessionIds={m().sessionIds}
            onClose={() => setFolderCtxMenu(null)}
          />
        )}
      </Show>
    </div>
  );
}
