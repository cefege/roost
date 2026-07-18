# claude.ai/code UI audit — hand-off doc

We're porting claude.ai/code's chat UI patterns into our open-source Mac app at `/Users/you/Code/roost/apps/desktop_mac`. The terminal mode stays as an escape hatch; the chat UI is the primary surface. Goal: feature-parity-where-it-matters with claude.ai/code, eventually exceed it (open source, more functionality).

## What's already known (from a saved app-shell MHTML)

A page snapshot of claude.ai/code is at `/tmp/cc-mhtml/part-1.html` (extracted from `/Users/you/Downloads/Claude Code.mhtml`). It's just the app shell — no menus opened, no sessions clicked into — so the static structure is captured but flows aren't.

### Layout

Three vertical regions:
1. **Sidebar** (collapsible, drag-resizable). Sections: header + nav, Pinned, Recents, footer (user menu). Resize handle is a separate element. "Collapse sidebar" button.
2. **Primary pane** (`aria-label="Primary pane"`) — current session's transcript + composer. Header strip with breadcrumb, status, action buttons.
3. **Side chat** (`aria-label="Side chat"`, toggleable, "Close side chat" button) — secondary docked chat panel.

There's also a tile/split-pane system inside Primary, keyboard-driven: hint label says "Arrow keys move the tile. Perpendicular arrows preview a split; press Enter to commit or Escape to cancel."

### Sidebar — session rows

```
[status icon]  [truncated title with fade mask]  [⋯ More options on hover]
```
- Status: `Idle` (static icon) or `Running` (3 pulsing dots — class `dframe-dot` × 3, same anim as a "thinking" indicator).
- Title uses CSS mask gradient for right-edge fade (not ellipsis — preserves character count).
- Rows are `df-drag-shiftable` — drag to reorder.
- "Pinned" and "Recents" section headers are collapsible buttons (`aria-expanded`).
- "View all" appears on Recents hover (only ~25 items shown by default).
- Session IDs: `data-row-key="code:session_01XwQCB9..."` — Claude's own session DB (separate namespace from `~/.claude/projects/<encoded>/<uuid>.jsonl`).

### Sidebar — chrome

Top buttons: `Add` (new session), `Search`, `Filter`, `Notifications (F8)`, `Appearance`, `Views`, `More navigation items` (overflow). Footer: `user-menu-button` testid.

### Primary header

- Session title (likely click-to-edit; Session actions menu has rename)
- `Session actions` menu (rename/archive/fork/etc.)
- `Share` button
- `Transcript view mode` dropdown (unknown options — needs live click)
- `Side chat` toggle

### Transcript

- Container: `data-testid="epitaxy-virtual-transcript"` — virtualized list.
- Markdown body (GFM tables confirmed in rendered content).
- Per-message action toolbar on hover: `Copy message`, `Pin as chapter` (toggle with `aria-pressed`).
- Tool-call chips: compact verb-list inside one chip ("Ran a command, read a file, edited a file").
- **Inline PR refs**: `aria-label="#114 · Merged"` — detects "PR #N" and decorates inline with GitHub PR state.

### Composer

- contenteditable Tiptap/ProseMirror with `aria-label="Prompt"`, placeholder: `Type / for commands` (slash-command palette).
- `Send` button right of the editor.
- Composer footer row:
  - Left: Dictation mic + Dictation settings + "Press and hold to record"
  - Right: model picker chip showing `Opus 4.7 · High` (model + effort), Usage gauge `aria-label="Usage: plan 21%"` (context window % used)

### Floating UI

- `Scroll to bottom` button (appears when scrolled up)
- `Notifications` toast region (top-right)

## Open questions — what couldn't be extracted from the static page

These need live navigation via humanchrome MCP. Click and observe:

1. **Slash-command palette** — type `/` in the composer; capture the full command list and structure
2. **Session actions menu** — click ⋯ on a session row; list items, separators, dangerous-action styling
3. **Transcript view mode** dropdown — capture the options + what each does visually
4. **Filter** in the sidebar — open it; capture filter dimensions (date / project / status?)
5. **Search** in the sidebar — open it; observe instant-search behavior, what's indexed
6. **Pin as chapter** flow — pin a message; observe the chapter rail or whatever surfaces
7. **Side chat** — toggle it open; how does it interact with the primary chat, can both run independently
8. **Share** flow — click Share; capture the share modal (public link? team-only? expire?)
9. **Usage gauge popover** — click the donut; what breakdown does it show (cache vs input vs output tokens?)
10. **Tile/split-pane** — try the keyboard hint (arrow + Enter); capture how multiple sessions render side by side
11. **PR ref hover/click** — find a `#NN` ref in a transcript; hover for tooltip, click for action
12. **New session flow** — click `Add`; observe the picker (project/folder/empty?)
13. **Session row context menu** vs ⋯ menu — are they the same? right-click on a row
14. **Tool chip expanded state** — click a "Ran a command" chip; capture the expanded details panel for Bash, Edit, Read, Write
15. **Appearance** — capture theme options + density
16. **Notifications** — F8 it open; capture what kinds of notifs exist

## Priority order for porting (already triaged)

Already done in the Mac app:
- ✅ Chat panel with role bubbles + markdown
- ✅ Tool-chip coalescing with verb-list summary
- ✅ Three-pane mode toggle (Terminal / Split / Chat)
- ✅ E2E test harness in `apps/desktop_mac/test_e2e/` (8 tests green)

Next (high value, low effort):
1. Per-message **Copy** button on hover
2. **Status indicator on the tab/session chip** (3-dot animation when status is `.thinking`)
3. **Scroll to bottom** button when scrolled up
4. **Model + usage chip** in composer footer (data lives in JSONL `message.model` + `usage.*`)
5. Default pane mode = `.chat` for Claude tabs (not `.split`) — user wants chat-primary

Medium value:
6. Sidebar **Search** + **Filter** (Mac app sidebar already has the data via `list-sessions` RPC)
7. **Slash-command palette** in composer
8. Inline **PR/issue refs** decoration
9. Dictation (NSSpeechRecognizer wrapper)
10. **Pin as chapter** (needs schema change in `ChatTranscript`)

Large effort, defer:
11. Side chat
12. Tile/split-pane system
13. Session-level pinning in sidebar

## File pointers

- Mac app chat panel: `apps/desktop_mac/Sources/IdeaDesktop/Views/ChatPanel.swift`
- Tool-chip rendering: `apps/desktop_mac/Sources/IdeaDesktop/Views/MessageBubble.swift`
- Pane mode toggle: `apps/desktop_mac/Sources/IdeaDesktop/DetailView.swift`
- Chat data model: `apps/desktop_mac/Sources/IdeaDesktop/Services/ClaudeChatClient.swift`
- App state singleton: `apps/desktop_mac/Sources/IdeaDesktop/AppState.swift` (has `paneMode(for:)`)
- E2E tests: `apps/desktop_mac/test_e2e/` (use `make test-e2e-fast` to verify changes)
- Saved app-shell HTML: `/tmp/cc-mhtml/part-1.html` (mhtml at `/Users/you/Downloads/Claude Code.mhtml`)
