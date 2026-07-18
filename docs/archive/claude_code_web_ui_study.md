# Claude Code Web UI — Study & Action Plan

> Captured 2026-05-17 against `claude.ai/code` (Research preview) — Opus 4.7. Source screenshots in `/tmp/cc_*.jpg`. Logged-in user: cefege (Max plan).

---

## 1. Information architecture

URL paths discovered:

| Path | View |
|---|---|
| `/code` | Home — "Welcome back" with Sessions list + Pull requests list |
| `/code/new` | Redirects to `/code` (no separate new-session view; home *is* the launcher) |
| `/code/routines` | Routines (templated scheduled/triggered agent jobs) |
| `/code/session_<ULID>` | Session detail — transcript + composer + optional right-side panel |
| `/code/<customize>` | Customize (skipped per user direction) |

Top-level nav (sidebar): **New session**, **Routines**, **Customize**, **More** (collapsible). The active route gets a light bg pill (`bg-bg-200`).

Two distinct asset types live side by side at the top level:
- **Sessions** — one-off agent jobs with state (`Needs input` / `Ready for review` / running).
- **Routines** — recurring or webhook-triggered templates. Cron or event-driven.

Pull requests show up as a separate section on home — Claude Code is aware of the user's GitHub PRs and surfaces them alongside its own sessions, with size badges (`XS`/`S`/`M`/…) and the same `Ready for review` status vocabulary as sessions. Implies a unified review queue.

---

## 2. Home (`/code`)

```
┌──────────────────────────────────┬──────────────────────────────────────────┐
│ Claude Code  [research preview]  │                                          │
│ ✜ New session  (selected)        │   ✺  Welcome back, User                 │
│ ⚡ Routines                       │                                          │
│ 💼 Customize                      │   Sessions                               │
│ ▾ More                            │   ● Needs input    Organize freelancer …│
│                                  │   ● Ready for review  Run E2E …         │
│ Recents  [⇅ filter]              │   ● Ready for review  Fix tab ownership │
│ ⋯ Debug service worker import()  │                                          │
│ ○ Process YouTube channel content│   Pull requests                          │
│ 🟢 Run E2E verification …         │   ● Ready for review  docs(backlog) #204│
│ ○ server-a-local-peppy-pond  │                                          │
│ ○ mac-app-e2e-testing           │                                          │
│ …                                │                                          │
│                                  │   ┌─────────────────────────────────┐    │
│ User · Max ▾           [⌨]    │   │ ☁ Default  + Select repo…       │    │
│                                  │   │ Describe a task or ask a question│↵│
│                                  │   │ Accept edits  +  🎤 ▾    Opus 4.7│    │
│                                  │   │                          1M · High│    │
│                                  │   └─────────────────────────────────┘    │
└──────────────────────────────────┴──────────────────────────────────────────┘
```

- **Sidebar header**: `Claude Code` wordmark + grey `Research preview` pill + two icons (sidebar collapse, search).
- **Sidebar nav** (column of label+icon rows, full-width pill on hover, selected = `bg-bg-200`):
  - `+ New session` (currently selected on home)
  - `⚡ Routines`
  - `💼 Customize`
  - `▾ More` (collapsible group toggle — opens to additional items)
- **Recents** list with a tiny filter/sort icon. Two row glyphs:
  - `⋯` = ambiguous (possibly "more" / unread / pinned)
  - `○` = open circle (regular)
  - `🟢` git-branch icon = item bound to a real branch
- **Bottom-left user chip**: `User · Max ▾` + a `[⌨]` keyboard-shortcuts icon.

**Main pane:**
- Greeting `✺ Welcome back, User` (coral brand glyph).
- **Sessions** list rows (`<li class="group">` with `<button>` inside):
  - Status dot+text: yellow `● Needs input`, blue `● Ready for review`.
  - Title.
  - Right-aligned `org/repo` + relative time (`2h`, `10m`, `17h`).
  - Chevron `›` at far right.
- **Pull requests** list rows: same shape + `#NUM` + size badge (`XS` etc.).
- **Composer** (sticky bottom):
  - Above input: `☁ Default` env pill, `+ Select repo…` pill.
  - Input: TipTap (ProseMirror) editor, placeholder "Describe a task or ask a question", return-glyph at right.
  - Below input: `Accept edits` mode dropdown, `+` attachment, `🎤` mic, `▾` more.
  - Bottom-right model indicator: `Opus 4.7 1M · High` + tiny spinner (usage status).
  - Coral pixel-mascot at far right (whimsy).

---

## 3. Session detail (`/code/session_<ULID>`)

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ☰  📓 knowledge / Organize freelancer folders by topic ▾    [⊟] [↑] [⊞ ▾]    │
├──────────────────────────────────────────────────────────────────────────────┤
│  Ran Classify audio files: transcribed vs orphan ›                           │
│  Found:                                                                       │
│    • 365 audio files with matching .md transcripts → 13.35 GB, safe to delete│
│    • 47 orphans (no transcript yet, mostly .part partial downloads) → leave  │
│  Want me to delete the 365 transcribed-audio files?                          │
│  Asking Delete audio ›                                                        │
│  Delete 365 audio files that already have transcripts (~13.35 GB)?           │
│  ✺                                                                            │
│                                                                               │
│  ┌─────────────────────────────────────────────────────────────────────────┐ │
│  │ ● Delete 365 audio files that already have transcripts (~13.35 GB)?   ×│ │
│  │                                                                          │ │
│  │ Yes, delete them                                                      [1]│ │
│  │   Remove all 365 .mp3/.webm/.m4a/.opus files where … exists.           │ │
│  │ Yes, and also clean up the 47 orphan .part files                      [2]│ │
│  │   Delete the 365 transcribed-audio files plus the 47 partial…         │ │
│  │ Show me the full list first                                           [3]│ │
│  │   Print all 365 paths before deleting anything.                       │ │
│  │ Other                                                                 [4]│ │
│  │   [ Type your own answer here                                         ] │ │
│  │                                                            Skip  Submit↵│ │
│  └─────────────────────────────────────────────────────────────────────────┘ │
│                                                                               │
│  ┌─────────────────────────────────────────────────────────────────────────┐ │
│  │ /                                                                      ↵│ │
│  │ Accept edits  +  🎤 ▾                              Opus 4.7 · High    ⊙│ │
│  └─────────────────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Header
- Collapsed-sidebar hamburger `☰` on the left.
- Breadcrumb-style title: `📓 knowledge / <session title> ▾` (blue book icon = repo; caret opens rename/options).
- Top-right toolbar (3 small icon buttons):
  - `⊟` — share/copy snapshot (popover).
  - `↑` — upload/export.
  - `⊞ ▾` — **side-panel dropdown**: opens to `Diff` (`^⇧D`), `Background tasks`, `Plan`.

### Transcript model
- **Collapsed turn** rendered as one-line: `<verb> <object> ›` (e.g. `Ran Classify audio files…`, `Asking Delete audio …`). Clicking expands.
- Assistant messages render full markdown (lists, inline `code`, arrows).
- Turn separator: the coral `✺` glyph alone on its own line.
- Inline tool/state badges are minimal — the model output is the surface, not a chat avatar.

### Interactive prompt card (the key differentiator)
- Yellow dot title + bold question.
- 1–4 numbered options, each with a heading + greyed subtitle.
- Number `[1] [2] [3] [4]` pinned to right edge — **keyboard-actionable** (the `1` ... `4` keys submit that choice).
- `Other` option exposes an inline `<input>` for free-form reply.
- `Skip` + `Submit ↵` actions bottom-right. Submit becomes active only when a choice is made.
- `×` dismisses the card.

### Composer (session variant)
- Simpler than home: placeholder is `Type / for commands`.
- Same `Accept edits / + / 🎤 / ▾` controls; same model indicator.
- Model indicator inside a session reads `Opus 4.7 · High` (no `1M`); on home it's `Opus 4.7 1M · High`. Different default per surface.

---

## 4. Composer interaction taxonomy

### Mode picker (`Accept edits` ▾ — shortcut `⌥⌘M`)
| # | Mode | Behavior (inferred) |
|---|---|---|
| 1 | Ask permissions | Stops before each write/exec for confirmation |
| 2 | **Accept edits** ✓ default | Auto-accepts edits, still asks for exec/dangerous ops |
| 3 | Plan mode | Plans without executing |
| 4 | Auto mode | Fully autonomous, no prompts |

Numbered shortcuts (1-4) act as quick-select inside the menu.

### Model picker (`Opus 4.7 · High` ▾)
Two sub-menus inside one popover:

**Models** (`⇧⌘I`):
1. **Opus 4.7** ✓ (default in-session)
2. Opus 4.7 **1M** (1M-token context variant; default on home composer)
3. Sonnet 4.6
4. Haiku 4.5

**Effort** (`⇧⌘E`):
- Low / Medium / **High** ✓ / Max — reasoning depth ("thinking budget").

### Plan usage tooltip (hover the spinner next to model indicator)
- `Plan usage →`
- `5-hour limit` 31% bar
- `Weekly · all models` 25% · resets 2d
- `Weekly · Claude Design` 0%
- `Sonnet only` 1% · resets 2d

Four orthogonal budgets. The `Sonnet only` bucket is a fallback pool once main quota is exhausted.

### Environment picker (`☁ Default` ▾)
Three execution surfaces:

1. **Local** — "Desktop only" (greyed unless you're in the desktop app).
2. **Cloud** — `☁ Default` ✓ + per-env gear; `+ Add cloud environment…` to define more. This is the hosted sandbox.
3. **Remote Control** — `Set up Remote Control`: "Run `claude rc` on your machine to code from here." Lets the web UI drive a local CLI agent.

### Repo picker (`+ Select repo…`)
- Scrollable list of all user GitHub repos (e.g. `cefege/humanchrome`, `cefege/flame`, `cefege/knowledge`, …).
- Bottom search box `🔍 Search repos…`.
- Selecting a repo scopes the new session to it (forms the `org/repo` chip shown on session rows).

### Slash commands
- Placeholder hint `Type / for commands` in the session composer.
- Typing `/` alone in our test didn't open a popover — likely fires only after a letter, or only inside a started session with allowed surface.

### Other composer affordances
- `+` button → attachment / file upload (not opened in our run).
- 🎤 + ▾ → voice input (recording state turns the mic icon blue). Caret next to mic likely picks input language/device.

---

## 5. Right-side panel

Opens via the top-right `⊞ ▾` dropdown. Three tabs:

| Tab | Shortcut | Contents |
|---|---|---|
| **Diff** | `^⇧D` | Header `📁 main → working tree ×`. Shows git diff of session-modified files. Empty state: "No changes to show." |
| **Background tasks** | — | (Not opened; likely long-running shell commands / detached processes.) |
| **Plan** | — | (Not opened; likely the plan-mode artifact / todo list.) |

Layout: fixed-width column on the right (~370px), independent vertical scroll, header has its own close `×`. Resizer handle on its left edge (`cursor-col-resize`). When open, the main transcript narrows to accommodate it — true 3-column layout (sidebar / transcript / panel).

---

## 6. Routines page (`/code/routines`)

Even though we won't deep-dive Routines, it sets context:

- Header: `Routines` + `+ New routine` button.
- 2-col gallery of 8 starter templates: Briefing, Email triage, System health check, Issue triage, PR review digest, Dependency update check, Release notes drafter, Flaky test tracker.
- Each card: icon, name, one-line description, schedule (`weekdays 7:30`, `daily 7:00`, etc., or `trigger: PR closed`), integration badges (Google Calendar, Gmail, Slack, PagerDuty, Datadog, Sentry, Linear).
- Two trigger models: **cron** and **event** (PR-closed webhook).
- Empty state line for the user's own list: "No routines yet."

---

## 7. Design tokens & patterns

### Color & theme
- Dark-mode only (no light toggle surfaced).
- Layered surfaces with very subtle tonal steps: page bg ≈ `#0F0F0F`, sidebar bg ≈ `#181818`, selected/hover ≈ `#222`. Separation by spacing & alignment more than by border.
- Accent: coral/orange (`✺` glyph, mascot). Used sparingly — only the brand mark and the per-turn separator.
- Status dot palette:
  - **Yellow** = needs human input
  - **Blue** = ready for review
  - (Green/red not observed in this run; presumably success/error)

### Typography
- Sans-serif geometric (Claude's house font).
- Heading scale is restrained — the only big heading is the home greeting; everything else lives in `text-body` / `text-footnote`.
- Inline code: `monospace` with `bg-bg-300` pill, e.g. `.md`, `.part`, `claude rc`.

### Component grammar
- **Pills** for every chip-shaped control (env, repo, mode, model, status). Consistent height ≈ 24-28px, `rounded-r5`.
- **Numbered option lists** with keyboard shortcuts on the right (`[1]`-`[N]`).
- **Popovers** anchor at the trigger, drop above/below depending on viewport, share a single design (`bg-bg-200`, ~10px radius, no border, soft shadow).
- **Sectioned popovers**: group label in small caps + items, e.g. Models/Effort in the model picker; Local/Cloud/Remote Control in the env picker.
- **Tailwind + custom design-tokens**: utility classes show CSS custom-property names (`var(--df-row-h)`, `var(--df-hover)`, `var(--text-uncontained-selected)`) — a design system layered over Tailwind.

### Keyboard-first
Every interactive surface has a visible shortcut hint (` ⌥⌘M `, ` ⇧⌘I `, ` ⇧⌘E `, ` ^⇧D `, ` 1 ` …` 4 `). Strong signal: this UI is built for power users who never want to touch the trackpad.

### Editor
- TipTap / ProseMirror (`.tiptap.ProseMirror` classes). Two instances on session view (one composer at bottom, one inline "Type your own answer here" inside prompt cards).

---

## 8. Mental model

```
User
 ├── Sessions  ← discrete one-shot agent runs (status, repo, transcript)
 │     └── repository
 │           └── environment (Local | Cloud sandbox | Remote-controlled local)
 ├── Routines  ← cron- or webhook-triggered agent jobs from templates
 └── Pull requests  ← surfaced from GitHub for the same review queue
```

Three runtime surfaces is the headline insight: the web UI is **not** the agent; it's a control plane that can drive (a) Anthropic-hosted sandbox VMs, (b) a CLI on the user's own machine (`claude rc`), or (c) the desktop app's local runtime — without changing the chat surface.

Sessions vs. Routines mirror the chat-vs-cron split: same agent, different trigger. Both produce transcripts of identical shape; both can be reviewed from the same queue.

Prompt cards (numbered choices with keyboard shortcuts) signal that the *human-in-the-loop checkpoint* is a first-class UI primitive, not just a "yes/no" inline button. This is where most of the UX care has been spent — they're large, descriptive, copy-paste-able, with a free-form `Other` escape hatch.

---

## 9. Action plan for `apps/desktop_mac` + `apps/main_node`

The repo already has a "claude-mode: native chat panel that activates when claude is detected" commit (dc8feb0) and the migration off the old multiplexer-backed session model to keeper is done (39fc3b4). This study confirms the direction is right, BUT our shape is different from Claude Code web in three load-bearing ways — locked in by user direction:

**Core constraints (from user, 2026-05-17):**

| Web Claude Code | Our app |
|---|---|
| Cloud sandbox VM or hidden runtime | **A session IS a terminal pane with `claude` running in it.** Terminal is the canonical state; native UI is a *view* onto it. |
| Cloud / Local / Remote Control envs | **Local Macs only**, but multiple of them on a network (Mac mini + MacBook + …). Env picker becomes a **machine picker**. |
| Sessions + Routines + PRs share one queue | **Sessions only.** No Routines, no PR queue. |

This narrows scope sharply. The phases below are reshuffled to reflect critical-first based on your tagging.

### Phase A — Session model (CRITICAL, was Phase 5)
The home view. Drives everything else.

- [ ] **Sessions list as home**, rows = (status dot, title, repo chip, machine chip, age, chevron). One row per `(machine, repo, terminal pane with claude)` triplet.
- [ ] **Status vocabulary** emitted by the agent loop (or detected from terminal output):
  - `Running` — claude is producing tokens / running a tool
  - `Needs input` — claude printed a question and is awaiting a reply
  - `Ready for review` — claude finished a chunk of work, idle waiting
  - `Done` — terminal exited or session was archived
- [ ] **Persistence**: a session is `(machineId, repoPath, tmuxOrKeeperPaneId, title, createdAt, lastActivityAt)`. The keeper daemon already gives us pane survival across detaches.
- [ ] **URL/route**: `/session/<id>` opening the session view; `/` shows the list.
- [ ] Drop the PR queue, drop Routines.

### Phase B — Interactive prompt card (CRITICAL, was Phase 2)
The highest-leverage primitive Claude Code has. Right now in raw terminal this renders as a numbered list — we already see it in the transcript but cannot click it. Native overlay should intercept.

- [ ] **Detection**: parse terminal output for claude's "question + N options" pattern. Reliable because claude's prompt cards in the TUI have a stable shape (numbered options, "Type / for commands" cue). The "claude-mode: native chat panel that activates when claude is detected" commit is the foundation — extend it to also activate when a prompt card appears in the stream.
- [ ] **Component contract** (`IdeaPromptCard` in SwiftUI for desktop_mac, and a LiveView component for main_node):
  - title + status dot (color: yellow if `needs input`)
  - 1-N option rows: title + subtitle (subtitle parsed from the line after the option in claude's output)
  - keyboard `1`-`N` selects (selection writes that digit to the underlying PTY, then Enter)
  - `Other` row with inline `<input>` — typed text is written to the PTY, then Enter
  - `Skip` button — writes `Esc`
  - `Submit ↵` — writes the chosen digit + `\n`
  - `×` dismiss — collapses to one-liner in transcript, doesn't change the PTY
- [ ] **Replay-in-transcript on dismiss**: once answered (or dismissed), the card collapses to `Chose: <option title> ›` in the transcript history, mirroring web's collapsed-turn style.
- [ ] This is the **single most important component to build** because it's the place the web UI adds the most value over raw terminal — and it directly addresses why the user said "in terminal never saw it or the ui in terminal is better" for Phase 4 below.

### Phase C — Right-side panel (NEEDED, was Phase 3)
Three-pane: sidebar / transcript-over-terminal / optional right panel with resizer.

- [ ] First tab: **Diff** — runs `git -C <repoPath> diff` against working tree, syntax-highlighted. Show "No changes to show" when empty. Auto-refresh when the underlying terminal writes (debounced 500ms).
- [ ] Second tab: **Background tasks** — list of long-running shell commands the session has started (parsed from `&`-suffixed lines, `nohup`, or claude's own background-task tool output). Each row: command, started-at, status, last 20 lines of stdout. Click expands to full log.
- [ ] Third tab: **Plan** — render claude's plan-mode artifact (the to-do list / checklist claude produces in plan mode). Detected from the terminal stream.
- [ ] **Keyboard shortcuts**: `⌃⇧D` diff, `⌃⇧B` background tasks, `⌃⇧P` plan.
- [ ] **Resizer** with min/max widths; remembers last width per session.

### Phase D — Machine picker (refined Phase 4)
You flagged Phase 4 as "i don't really understand this" because web's `Cloud / Remote Control` envs don't exist in your model — and the terminal version (TUI claude) is single-machine. Your direction: **just local Macs, but multiple of them**. So the picker reduces to:

- [ ] **`Machine` chip in the composer** (replaces web's `☁ Default` env pill). Default = `This Mac`. Dropdown lists other reachable Macs.
- [ ] Discovery: Bonjour/mDNS on the local network for peers running the `idea` desktop_mac daemon. (Already partial infrastructure: `MainNodeAPI`, `IdeaTestServer`.)
- [ ] When a session is created with a non-local machine, the keeper-pane gets spawned on that remote Mac via SSH/MainNodeAPI; the local UI mirrors its output.
- [ ] **No cloud, no Anthropic-hosted runtime.** That's a strict scope cut.
- [ ] **Why this matters**: it's what lets you start a session on the Mac mini at home from the MacBook while traveling, without losing the "it's just a terminal with claude" model.

### Phase E — Keyboard ergonomics (CRITICAL, was Phase 6)
Mirror Claude Code's shortcuts so muscle memory transfers:

| Shortcut | Action |
|---|---|
| `⌥⌘M` | Mode menu (Ask permissions / Accept edits / Plan / Auto) |
| `⇧⌘I` | Model menu (Opus 4.7 / Opus 4.7 1M / Sonnet 4.6 / Haiku 4.5) |
| `⇧⌘E` | Effort menu (Low / Medium / High / Max) |
| `⌃⇧D` | Toggle Diff panel |
| `⌃⇧B` | Toggle Background tasks panel |
| `⌃⇧P` | Toggle Plan panel |
| `1`-`9` | Pick numbered option in prompt card |
| `Enter` | Submit prompt card |
| `Esc` | Skip prompt card |
| `⌘K` | Search sessions (sidebar search icon) |

Surface the shortcut hint inside every popover (e.g. `⌥⌘M` shown in the mode-menu header), not buried in a help screen.

### Phase F — Visual polish (clarifying Phase 7)
You said this isn't clear. Concretely, here's what to copy:

- **Layered dark surfaces, 3 tonal steps:**
  - Page background ≈ `#0F0F0F` (near-black)
  - Sidebar / panels ≈ `#181818` (one step up)
  - Hover / selected ≈ `#222222` (two steps up)
  - In SwiftUI: define `Color.bg100`, `Color.bg200`, `Color.bg300` extensions; use the same names in main_node's CSS as `--bg-100/200/300`. Sidebar uses bg200, hovered rows use bg300. Done.
- **Pill controls** — every chip-shaped affordance (mode, model, machine, status):
  - Height: 24-28px
  - Padding: 8-12px horizontal
  - Radius: 6-8px (`rounded-md` in Tailwind)
  - In SwiftUI: a reusable `IdeaChip` view modifier.
- **Coral accent**, used *sparingly*:
  - Brand glyph (the `✺` between agent turns — copy this exact pattern instead of avatars/labels)
  - One small mascot in the bottom-right corner of the composer (we can ship our own pixel art — keeps the playful note without copying theirs)
- **Status dot palette** (3 colors, no more):
  - Yellow `#F5BE3A` — needs input
  - Blue `#5B9CFF` — ready for review
  - Green `#3FCF8E` — running (or in-progress)
- **Typography**: stick with system fonts (SF Pro on macOS, system stack in main_node). Restrained scale: one big heading per view, everything else body or footnote.
- **Spacing as separator, not borders** — almost no 1px borders in Claude Code. Use 16-24px gaps to separate regions.

The point of Phase F is **the UI looks built by the same team across desktop_mac and main_node** — same tokens in both surfaces.

### What we are explicitly NOT building (scope cuts from your feedback)
- ❌ Routines (cron/webhook templates) — not in scope
- ❌ Pull-request queue in the home view — not in scope
- ❌ Cloud sandbox runtime — local Macs only
- ❌ "Remote Control" (web-driving-CLI) — same reason; we're CLI-first already
- ❌ A separate `Customize` page — fold settings into a single keyboard-shortcuts cheat sheet + a per-session config inside the right panel

### Open questions still worth answering

#### Q1 — Slash-command inventory ✅ RESOLVED 2026-05-17

**Answer:** The commands are baked into the bundled JS inside the Claude Code binary, not in a sidecar config file — but the user's intuition was right that they change per release. We extract with `strings` against `/opt/homebrew/Caskroom/claude-code/<version>/claude`. Re-runnable script at `scripts/extract_claude_code_commands.sh`; snapshot at `docs/snapshots/claude_code_slash_commands_2.1.132.tsv` (50 commands).

**Commands shortlisted for milestone-1 in our composer** (the ones we'll actually need first):

| Command | Description | Phase |
|---|---|---|
| `/clear` | Start a new session with empty context; previous session stays on disk (resumable with /resume) | A |
| `/compact` | Free up context by summarizing the conversation so far | A |
| `/resume` | Resume a previous conversation | A |
| `/diff` | View uncommitted changes and per-turn diffs | C |
| `/context` | Visualize current context usage as a colored grid | B |
| `/plan` | Enable plan mode or view the current session plan | B |
| `/recap` | Generate a one-line session recap now | A |
| `/copy` | Copy Claude's last response to clipboard (or /copy N for the Nth-latest) | A |
| `/stop` | Stop this background session; transcript and worktree are kept | A |
| `/agents` | Manage agent configurations | E |
| `/hooks` | View hook configurations for tool events | E |
| `/help` | Show help and available commands | A |

The full 50-command list is in the snapshot. Re-run the extraction after each `brew upgrade claude-code` to refresh.

**Implications for our composer:**
- The slash-command surface is *long-tail*. Don't try to mirror all 50; build a generic registry where each command is one entry with `{name, description, dispatch}` and ship the 12 above first.
- Several commands (`/teleport`, `/web-setup`, `/install-github-app`, `/setup-bedrock`, `/setup-vertex`, `/install-slack-app`, `/loops`, `/daemon`, `/remote-env`) are Anthropic-specific or cloud-specific and we explicitly skip them.
- A few are worth lifting wholesale because they map to our existing infra: `/diff`, `/hooks`, `/agents` map to features we already have or want.

#### Q2 — Right-panel populated states ✅ RESOLVED 2026-05-17

**Captured both populated states. Screenshots: `/tmp/cc_bgtasks2.jpg`, `/tmp/cc_plan2.jpg`.**

##### Background tasks panel — populated shape

Header: `Background tasks   ×`

Body is **grouped by status** (we saw `Completed`; presumably `Running`, `Failed` too):

```
Background tasks                     ×
─────────────────────────────────────
Completed
 ✓ Find eagerTools array          ›
   [Bash · Completed]
```

Each row:
- Status glyph (✓ for completed)
- **Task title** = a short noun-phrase describing what claude was doing
- Right-side chevron `›` (expand to see full log)
- **Subline**: `<ToolName> · <Status>` pill — `Bash · Completed`, presumably `Bash · Running`, etc.

**Data contract for our `BackgroundTasksPanel`:**
```
{
  groups: [
    {
      status: "Completed" | "Running" | "Failed",
      tasks: [
        {
          id: string,
          title: string,              // "Find eagerTools array"
          tool: "Bash" | string,      // sub-pill left
          status: string,             // sub-pill right (mirrors group)
          startedAt: Date,
          endedAt?: Date,
          output?: string             // shown on expand
        }
      ]
    }
  ]
}
```

##### Plan panel — populated shape

Header: `Plan   [⊟]  ×` (the `[⊟]` icon is probably copy-to-clipboard or share)
Subhead: `ⓘ Select any text to leave a comment for Claude`

Body is **not a todo list — it's a structured markdown document**:

```
Plan: Multi-instance bridge routing + ship IMP-0110 / 0112 / 0113

Context
─────────
The unattended-E2E pipeline shipped in IMP-0109/0111 works …
[long paragraph with inline code: `chrome.runtime.connectNative`, `listen({port:12306})`]
[file:line refs: `app/native-server/src/server/index.ts:521-532`]
[link: #203]

The fix is small but load-bearing: …

This plan also folds in three already-filed proposed items:
─────────
• IMP-0112 — role+name resolver. …
• IMP-0113 — …
• IMP-0110 — …
```

What it actually contains:
- A **title** (bold first line)
- Multiple **section headers** (`Context`, `Proposed items`, etc.)
- Rich markdown body: inline code, bold, italic, file:line refs, links to issue IDs (`#203`, `IMP-0112` → backlog), code blocks
- **Text-selection-to-comment** affordance — the user can highlight any span and leave a comment for claude to react to

**Data contract for our `PlanPanel`:**
```
{
  title: string,
  body: Markdown,               // full markdown, render with our existing markdown component
  comments: [                   // user comments anchored to text ranges
    { id, anchorText, anchorOffset, body, createdAt }
  ],
  annotationsEnabled: boolean   // toggle the "Select any text…" subhead
}
```

##### Implications for Phase C
- **Plan is a *document*, not a checklist.** Build a markdown-renderer that supports rich content + inline issue/file links, not a `<ul>` of checkboxes.
- **Background tasks is a *grouped list*** — group-by-status is the killer organizing principle, not a flat list.
- The "Select text → comment" interaction in Plan is a **significant secondary feature** — leave it for Phase C+1; rendering is enough for v1.
- Bonus: this same session showed an **inline PR card** at the bottom of the transcript (`📂 204 · humanchrome · docs/imp-0120-sw-reload-drops-mcp-clients · +15 −0 · ● CI ▾`). Different rich-element type — Claude Code knows how to render a PR reference inline with diff stats + CI status. Worth a separate component (`InlinePrCard`) if we want parity.

##### Bonus visual finding: user messages render as blue pills
On the `Re-auth gws and update email status` session I navigated through, user-typed prompts (`test`, `/clear`) showed as **blue rounded pills** inline in the transcript — not avatars, not "User:" prefixes. Matches our discovery of agent turns being separated by the coral `✺` glyph. The pattern is consistent: visual hierarchy via shape and color, not labels.

#### Q3 — Prompt-card overlay strategy in `claude-mode` ✅ RESOLVED 2026-05-17

**Decision: Option A — replace lines in-place.**

The terminal renderer detects claude's question+options block and swaps those lines for a native `IdeaPromptCard` *at the same scroll position*. Transcript order is preserved 1:1; user feels the terminal "got smarter" rather than a separate panel appearing.

##### Implementation contract for `TerminalView.swift` + `ChatPanel.swift`

**Detection** (already partially in place via the `claude-mode` commit dc8feb0):
- Parse stream output for the prompt-card pattern. Claude's TUI prompt cards have a stable shape: a question line followed by `\n1. <option>\n2. <option>\n…` with a `❯` prompt at the bottom.
- When matched, mark the byte range `[startOffset, endOffset]` of the question + options block as an **overlay region**.

**Rendering**:
- The terminal renderer must support **overlay regions**: a contiguous byte range whose corresponding screen lines are *hidden* and replaced by a same-height native view inset into the scroll content.
- The native view (`IdeaPromptCard`) has a known intrinsic height (~N rows). The terminal must reserve exactly that vertical space so the surrounding lines don't reflow when the card mounts/unmounts.
- Strategy: maintain a list of `[{range, viewHeight, view}]` overlays. When the terminal lays out its grid, it skips rows that fall inside any overlay range and inserts the view at that y-offset.

**Interaction**:
- Keyboard `1`-`N` in the card → write that digit + `\n` to the underlying PTY.
- `Other` row → focus a native inline `TextField`; on submit, write the typed text + `\n` to PTY.
- `Skip` → write `Esc`.
- `×` dismiss → only collapses the card to a single-line `Chose: <option>` row; the PTY state is untouched until the user actually answers.

**Lifecycle**:
- When claude moves past the prompt (either user answered or claude printed a follow-up line), the overlay region is **finalized** — the card collapses to a one-line summary in the transcript (matching the web Claude Code "Asking <X> ›" collapsed-turn pattern).
- The original terminal bytes are kept in the scrollback (not deleted) so the user can scroll up and verify what claude actually printed.

##### Why this is the right call
- The web Claude Code UI is essentially Option A applied at the chat-message level — there is no terminal showing the raw question alongside the card. Mirroring that pattern at the *terminal* level gives the user a single source of truth for "what claude said and what I picked."
- It matches the existing `claude-mode` direction (chat panel that *activates when claude is detected*) — the panel is the terminal's smart layer, not a sibling.
- Option B (float above) was attractive only for implementation simplicity — but it requires deciding what happens when the user scrolls (does the card stay docked or scroll with the terminal?), and either answer is wrong. Option A makes scroll trivial: the card moves with its overlay range.

##### Files affected for the first implementation
- `apps/desktop_mac/Sources/IdeaDesktop/Views/TerminalView.swift` — add overlay-region tracking + rendering
- `apps/desktop_mac/Sources/IdeaDesktop/Views/ChatPanel.swift` — currently the "claude-mode" panel; extract the prompt-card rendering into its own `IdeaPromptCard` view so it can also be embedded inside the terminal as an overlay
- `apps/desktop_mac/Sources/IdeaDesktop/Services/PtyClient.swift` — write-back path for `1`-`N` / Other / Skip
- New: `apps/desktop_mac/Sources/IdeaDesktop/PromptCardDetector.swift` — pure-Swift parser that takes a stream of PTY bytes and emits `OverlayRegion` events when it matches claude's prompt-card shape

---

## Appendix — captured screenshots

| File | View |
|---|---|
| `/tmp/cc_home.jpg` | Home with 3 sessions + 1 PR |
| `/tmp/cc_more.jpg` | Routines page with 8 templates |
| `/tmp/cc_session_full.jpg` | Session detail with interactive prompt card |
| `/tmp/cc_acceptedits.jpg` | Mode picker open |
| `/tmp/cc_modelpicker.jpg` | Model + Effort picker open |
| `/tmp/cc_model.jpg` | Plan usage tooltip |
| `/tmp/cc_sidepanel.jpg` | Side-panel dropdown (Diff / Background tasks / Plan) |
| `/tmp/cc_diff.jpg` | Diff right panel open, "No changes to show" |
| `/tmp/cc_repo.jpg` | Repo picker |
| `/tmp/cc_env.jpg` | Environment picker (Local / Cloud / Remote Control) |
