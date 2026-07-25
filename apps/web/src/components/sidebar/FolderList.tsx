// Folder list — THE sidebar body (one mode; sidebarViewPrefs + the
// Status/Folder/Folders cycle were deleted 2026-07-04). ONE zone:
//   Folder rows — ONE row per (worker, folder) bucket, cell-grid model; the
//   folder's terminals/agents live in the TabBar. Sorted needs → running
//   → recency. needs-input surfaces IN the row (glyph → needs-input,
//   subtitle "Waiting on your input"); a click on a needy row navigates
//   to the folder's lead pane, the exact one waiting on you. The separate flat "Needs
//   you" strip was deleted 2026-07-07 — it reverted the sidebar to the
//   pre-tiling flat-per-session look whenever any agent blocked.
// Groups by the SAME folderKeyOf the TabBar uses, so the two always agree.
// Owns the sidebar keyboard surface: cursor order (folder leads),
// ⏎ activate, ⌘⇧U jump-to-unread, seen-seeding.
// Classes are `df-fld-*` (NOT `df-folder-*` — that prefix is the legacy
// MachineSection tree; distinct namespace avoids style collisions).
//
// Reads allSessions(); pure read of the store (no setStore).

import { batch, createComputed, createEffect, createMemo, createSignal, For, Show, onMount, onCleanup } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { A, useNavigate, useLocation } from "@solidjs/router";
import { rootStore } from "../../store/root.ts";
import { closeSidebar } from "../../store/uiStore.ts";
import { Button } from "../Settings/md/Button.tsx";
import { allSessions, activeSessionForPath, folderRowTargetId } from "../../store/selectors.ts";
import { mostRecentUnread } from "../../lib/attention.ts";
import { markSeen, seedSeenOnce } from "../../lib/sessionSeen.ts";
import { markReadForSession } from "../../lib/notifyStore.ts";
import { getLastSessionForFolder } from "../../lib/lastVisited.ts";
import { setJumpUnreadHandler } from "../../lib/keyboardShortcuts.ts";
import { cursorSessionId, setActivateHandler, setOrderedSessionIds } from "../../lib/sidebarCursor.ts";
import { relTimeSince } from "../../lib/relTime.ts";
import { folderKeyOf } from "../../lib/folderKey.ts";
import { colorForFp } from "../../lib/fpColor.ts";
import { buildFolderGroups, type FolderGroup, PR_CHECK_GLYPH, PR_CHECK_COLOR } from "../../lib/folderGroups.ts";
import { pushRecent } from "../../lib/sidebarRecent.ts";
import { isChatFolder, startQuickChat } from "../../lib/quickChat.ts";
import { scheduleClose } from "../../lib/pendingClose.ts";
import { closeLabelsFor, killAfterUndo } from "../../lib/closeSession.ts";
import { StatusGlyph } from "./StatusGlyph.tsx";
import { relTimeTickMs } from "./SessionRow.tsx";
import { FolderRowContextMenu } from "./FolderRowContextMenu.tsx";
import { FlatNewTerminal } from "./FlatNewTerminal.tsx";
import { FolderGlyph } from "../FolderGlyph.tsx";
import { IconButton } from "../Settings/md/IconButton.tsx";
import "@material/web/ripple/ripple.js";

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
  const activeId = createMemo(() => activeSession()?.id ?? null);

  const activeFolderKey = createMemo(() => {
    const s = activeSession();
    return s ? folderKeyOf(s) : null;
  });

  // First-run seed: once sessions have loaded, stamp them all seen so the
  // needs-state doesn't light up every pre-existing finished agent.
  // Idempotent (localStorage marker) — only fires the very first time.
  let _seeded = false;
  createEffect(() => {
    if (_seeded) return;
    const ids = allSessions().map((s) => s.id);
    if (ids.length === 0) return;
    seedSeenOnce(ids);
    _seeded = true;
  });
  // Mark the session you're viewing as seen — up to its latest output, so
  // re-runs on new last_message keep the stamp current while you watch.
  // Functional setter in markSeen → no self-subscription loop.
  createEffect(() => {
    const id = activeId();
    if (!id) return;
    void rootStore.sessions[id]?.agent?.last_message?.ts; // track new output
    markSeen(id);
    markReadForSession(id); // clear this session's bell/badge unread on visit
  });

  // Cursor ⏎ opens a session: same effect as a row click (push MRU + nav).
  onMount(() => setActivateHandler((id) => { pushRecent(id); navigate(`/s/${id}`); }));
  onCleanup(() => setActivateHandler(null));

  // ⌘⇧U → jump to the most-recently-active session that needs you.
  onMount(() => setJumpUnreadHandler(() => {
    const s = mostRecentUnread(allSessions());
    if (s) { pushRecent(s.id); navigate(`/s/${s.id}`); }
  }));
  onCleanup(() => setJumpUnreadHandler(null));

  // ── Folder rows ────────────────────────────────────────────────────
  // Keyed store instead of a memo: `<For>` keys by reference, and the old
  // memo allocated fresh FolderGroup objects per run — every invalidation
  // (any agent tick) tore down and recreated EVERY row's DOM. reconcile
  // keyed by `key` keeps object identity for unchanged rows (DOM survives);
  // changed fields flow as fine-grained store updates because the JSX reads
  // g.* off store proxies. FolderGroup is plain scalars only (leadId /
  // sessionIds, not live Session proxies) so the deep diff stays cheap.
  const [gs, setGs] = createStore<{ rows: FolderGroup[] }>({ rows: [] });
  createComputed(() => setGs("rows", reconcile(buildFolderGroups(), { key: "key" })));

  // Sidebar category toggle — Code (workspaces) vs Chat (quick AI sessions).
  // Default Code (first). The row list below filters to the active tab; the
  // two categories never mix in one view. isChatFolder is a pure cwd path
  // check (scratch dirs under ~/.roost/chats).
  const [sidebarTab, setSidebarTab] = createSignal<"code" | "chat">("code");
  const chatRows = createMemo(() => gs.rows.filter((g) => isChatFolder(g.spawnCwd)));
  const folderRows = createMemo(() => gs.rows.filter((g) => !isChatFolder(g.spawnCwd)));
  const visibleRows = createMemo(() => (sidebarTab() === "chat" ? chatRows() : folderRows()));

  // Keyboard cursor order = the VISIBLE rows top-to-bottom (active tab only).
  createEffect(() => {
    setOrderedSessionIds(visibleRows().map((g) => g.leadId));
  });

  // Extracted so both tabs render identical row chrome.
  const renderFolderRow = (g: FolderGroup) => {
    // Needy folder → jump straight to the pane waiting on you (g.leadId is
    // the neediest session). Else reopen the tab you were last on in this
    // folder (if still open), falling back to the lead. (lastVisited.)
    const targetId = () =>
      folderRowTargetId(g.key, g.attention, g.leadId, getLastSessionForFolder(g.spawnFp, g.spawnCwd));
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
      data-stage={g.glyphStatus ?? ""}
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
      <span class="df-leading">
        <StatusGlyph status={g.glyphStatus} isClaude={g.isClaude} />
      </span>
      <span class="df-flat-body">
        <span class="df-flat-top">
          <span class="df-label df-flat-headline" title={g.spawnCwd}>{g.name}</span>
          <Show when={g.unreadCount > 0}>
            <span class="df-flat-unread" data-testid={`folder-unread-${g.key}`}>{g.unreadCount > 9 ? "9+" : g.unreadCount}</span>
          </Show>
          <span class="df-flat-time">{(relTimeTickMs(), relTimeSince(g.latestActivity))}</span>
        </span>
        <Show when={g.subtitle}>
          <span class="df-flat-subtitle">{g.subtitle}</span>
        </Show>
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

  // Soft-close a chat bucket: hide every session in it immediately
  // (buildFolderGroups filters isPendingClose → row vanishes) and fire each
  // kill after its own 5s undo window. Chats live in a throwaway scratch
  // folder with no meaningful sibling, so a viewed chat lands Home (not
  // siblingOrHomeHref like SessionRow). No onUndo — un-hiding the row is
  // enough; a chat has no pane-tiling to restore.
  function closeChat(g: FolderGroup) {
    const viewed = g.sessionIds.includes(activeId() ?? "");
    batch(() => {
      for (const sid of g.sessionIds) {
        const s = rootStore.sessions[sid];
        if (!s) continue;
        scheduleClose(sid, closeLabelsFor(s), killAfterUndo(sid));
      }
      if (viewed) navigate("/");
    });
  }

  // Slim chat-tab row: single line (title + time) + an always-visible ✕. No
  // repo/branch/PR/server/pane-count — a quick AI chat has none of that.
  const renderChatRow = (g: FolderGroup) => {
    const targetId = () =>
      folderRowTargetId(g.key, g.attention, g.leadId, getLastSessionForFolder(g.spawnFp, g.spawnCwd));
    return (
    <A
      href={`/s/${targetId()}`}
      class="df-row"
      title={g.spawnCwd}
      data-density="flat"
      data-chat="true"
      data-testid={`folder-row-${g.key}`}
      data-selected={activeFolderKey() === g.key ? "focused" : ""}
      data-cursor={cursorSessionId() === g.leadId ? "on" : undefined}
      data-stage={g.glyphStatus ?? ""}
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
      <span class="df-leading">
        <StatusGlyph status={g.glyphStatus} isClaude={g.isClaude} />
      </span>
      <span class="df-flat-body">
        <span class="df-flat-top">
          <span class="df-label df-flat-headline">{g.subtitle || "New chat"}</span>
          <Show when={g.unreadCount > 0}>
            <span class="df-flat-unread" data-testid={`folder-unread-${g.key}`}>{g.unreadCount > 9 ? "9+" : g.unreadCount}</span>
          </Show>
          <span class="df-flat-time">{(relTimeTickMs(), relTimeSince(g.latestActivity))}</span>
        </span>
      </span>
      <IconButton
        icon="close"
        label="Close chat"
        class="df-action df-action-always"
        data-testid={`chat-close-${g.key}`}
        title="Close chat"
        onClick={(e: MouseEvent) => { e.stopPropagation(); e.preventDefault(); closeChat(g); }}
        style={{ "--md-icon-button-icon-size": "14px" }}
      />
    </A>
    );
  };

  return (
    <div data-testid="folder-list">
      {/* Reuse the flat session-list card shell (df-flat-group + df-row[flat] +
          df-leading avatar + df-flat-body) so the folder view reads IDENTICALLY
          to the strip — same recessed cards, circular avatar, hover +
          selected states. Content maps folder→session: name→headline,
          activity/branch→subtitle, machine→supporting server line. */}
      <div class="df-fld-tabs" role="tablist" data-testid="sidebar-tabs">
        <button type="button" role="tab" class="df-fld-tab"
          data-active={sidebarTab() === "code" ? "true" : "false"}
          aria-selected={sidebarTab() === "code"}
          data-testid="sidebar-tab-code"
          onClick={() => setSidebarTab("code")}>Code</button>
        <button type="button" role="tab" class="df-fld-tab"
          data-active={sidebarTab() === "chat" ? "true" : "false"}
          aria-selected={sidebarTab() === "chat"}
          data-testid="sidebar-tab-chat"
          onClick={() => setSidebarTab("chat")}>Chat</button>
      </div>
      <Show when={sidebarTab() === "chat"}>
        <Button
          variant="tonal"
          icon="add"
          data-testid="sidebar-new-chat"
          aria-label="New chat"
          title="New chat"
          style={{
            display: "flex",
            width: "calc(100% - var(--md-space-4))",
            margin: "0 var(--md-space-2) var(--md-space-2)",
            "--md-filled-tonal-button-with-leading-icon-trailing-space": "16px",
          }}
          onClick={() => { closeSidebar(); void startQuickChat(navigate); }}
        >New chat</Button>
      </Show>
      <div class="df-flat-group">
        <For each={visibleRows()}>
          {(g) => (sidebarTab() === "chat" ? renderChatRow(g) : renderFolderRow(g))}
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
