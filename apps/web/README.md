# apps/web — the Solid SPA

The browser client. One Solid 1.x app on plain Vite (`bun x vite` on :5173 in dev, `bun x vite build`
into `apps/web/dist/` which coord serves in production). It paints terminal cells the worker already
rendered; it does not run a terminal core.

Path references are relative to `apps/web/` unless they start at the repo root (`apps/…`, `scripts/…`, `smoke/…`, `docs/…`).

## Entry point

`apps/web/index.html` loads `apps/web/src/entry.ts`, a dependency-minimal
security boundary. Startup order:

1. `captureAndScrubFragmentCredential()` synchronously removes URL-carried
   credentials before the SPA module graph loads.
2. `entry.ts` dynamically imports `apps/web/src/main.tsx`.
3. `applyTheme(loadTheme())` sets `data-theme` before first paint.
4. `installSignalShip()` + `installSpaDiag()` and the global
   error/rejection/chunk-recovery handlers install before render.
5. `claimTabIdentity()` settles this document's unique identity, then
   `applyTermFontSize()` sets terminal metrics.
6. `render(() => <App />, #app)` mounts Solid; leak watch and agent-config load
   follow.

`apps/web/src/App.tsx` is the root: `AppErrorBoundary` outermost, `ConnectionBanner` +
`VersionBanner` always mounted, `bootstrapSync()` (`apps/web/src/store/sync-bootstrap.ts`) called at
component-body time, then `<Router root={RootShell}>` from `@solidjs/router` — no SolidStart, no
Vinxi. `RootShell` is the always-mounted overlay tier (command palette, help overlay, toasts,
dialogs, `apps/web/src/components/UiBridge.tsx`, the smoke/shortcut router bridges) — it lives inside
`<Router>` because those pieces call `useNavigate()`.

Routes are declared once in `apps/web/src/routes.ts`; the URL is the source of truth for nav state.
`ROUTES.SESSION`, `TERMINAL_BY_FOLDER`, `WORKSPACE`, `WORKSPACE_TERMINAL`, `FILE` and `SEARCH` share
**one** `<Route>` whose `path` is an array, pointing at `apps/web/src/components/MainPane.tsx`. Never
split that into sibling `<Route>` entries: Solid Router keys the component instance to the route
*definition*, so separate entries remount `MainPane` — and the terminal deck under it — on every
`/s` ↔ `/file` ↔ `/search` crossing. Settings, `/pair`, `/help`, `/design` and `/browse` load through
solid `lazy()`, the repo's sanctioned code-split mechanism (its `ts-no-dynamic-import` exception).

## Module map

Rows cover the owned source/test directories. Within a row, a bare file name
lives in that row's directory; prefixed refs follow the convention above.

| Directory | Owns | Must not own |
| --- | --- | --- |
| `apps/web/src/` (root files) | `entry.ts` (credential scrub + deferred graph load), `main.tsx` (post-scrub bootstrap + mount), `App.tsx` (router + overlay shell), `routes.ts` (URL table), `connect.ts` (Connect-RPC client), `css-imports.d.ts` and `md-elements.d.ts` (ambient imports/elements) | feature-shaped UI |
| `apps/web/src/components/` | screens/dialogs; `NotificationDock.tsx` owns the ONE bottom overlay column (`ToastStack.tsx`/`ToastCard.tsx`, `UndoCloseBanner.tsx`, `TransferCard.tsx`, `PairRequestNotifier.tsx` render as its children and never position themselves); `DesignGallery.tsx` is the visual reference for theme tokens, shared primitives, and the canonical title/activity/sidebar/editor/status composition; `GlobalSearchPage.tsx` composes metadata/attention with `GlobalSearchContentResults.tsx`; `ArrangeMenu.tsx` exposes pane presets; `CellTerminal.tsx` composes `cell-terminal-types.ts`, `cell-terminal-runtime.ts`, `cell-terminal-input.ts`, `cell-terminal-presentation.ts`, `cell-terminal-viewport.ts`, `cell-terminal-renderer.ts`, `cell-terminal-interactions.ts`, and `cell-terminal-lifecycle.ts`; `cell-terminal-document-lifecycle.ts` fans one page-lifecycle listener set to mounted terminals; `TerminalStartupOverlay.tsx` is the ONE opening-terminal card, mounted by both `MainPane.tsx` and `CellTerminal.tsx`; `TerminalCard.tsx` is the compact deck's terminal card; `PaneTab.tsx`/`PaneTabList.tsx`/`paneTabRailScroll.ts` own tab-rail measurement and the overflow filter; `WorkerBrowsePage.tsx` composes the folder picker from `BrowseEntryList.tsx`, `BrowsePathBar.tsx`, `BrowseToolbar.tsx`, `NewFolderDialog.tsx`, `browseDirectoryListing.ts`, `browseNewFolder.ts`, `browsePickerKeys.ts`, and `browseBreadcrumbCollapse.ts` | global state, transport, or terminal cell parsing |
| `apps/web/src/components/layout/` | `AppShell.tsx` owns the canonical desktop workbench grid and compact/mobile shell; `WorkbenchTitleBar.tsx`, `WorkbenchActivityBar.tsx`, and `WorkbenchStatusBar.tsx` own truthful desktop chrome; `SidebarResizer.tsx` and `MobileSidebarDrawer.tsx` retain sidebar interaction seams; `MobileTopBar.tsx` owns compact route context | route-specific content |
| `apps/web/src/components/sidebar/` | machine / folder / session lists, sidebar search, row context menus, `ViewersChip.tsx` | per-view stores — selection and filtering derive from the URL and `rootStore` |
| `apps/web/src/components/Settings/` | settings shell/panes; `MachinesPane.tsx` owns workers, `DevicesPane.tsx` is the only identity surface, `settingsNavigation.ts` owns the single `SETTINGS_GROUPS` list | raw CSS values; panes compose `apps/web/src/components/Settings/md/` |
| `apps/web/src/components/Settings/md/` | one-component-per-file M3 primitives re-exported by `primitives.tsx`; `tokens.css` consumes canonical theme variables and `icon.css` styles icons; `Skeleton.tsx` is the shared loading placeholder | app state, data fetching, or token declarations |
| `apps/web/src/store/` | single reactive state: `root.ts`, selectors/mutations/projector, Sync leaves, terminal replica/view leaves (`terminal-stream-renewal-scheduler.ts` owns one document renewal timer and `terminal-stream-progress.ts` pushes chunk progress), pane/UI stores; `terminal-stream-transport.ts` is the dependency-free registration seam for the local worker transport and `terminal-stream-publication.ts` turns it into the single publication target per session, while `terminal-stream-retarget.ts` owns every generation/transport retarget and the view-id rotation it requires; `paneLayoutDocument.ts` is the portable-document adapter over the browser-local pane store; `agent-status.ts` owns epoch/occupant admission and retired-identity fencing; `auth-boundary.ts` owns the credential-boundary generation guard and authenticated-state teardown; `notifyTarget.ts` owns the single hovered-toast target signal | JSX or module-global socket/reconnect state |
| `apps/web/src/ws/` | the **outbound** halves of both terminal transports: `terminal-input-lanes.ts` owns the per-session input lane (caps, correlations, result timeouts) shared by Sync (`sync-outbound.ts`, which also routes each session to its transport) and the local worker socket (`local-terminal.ts` + `local-terminal-requests.ts` + `local-terminal-grants.ts`), plus smoke hooks | socket dispatch for Sync, inbound cell decoding, membership, or continuity — local frames enter the same `store/terminal-stream*.ts` replica |
| `apps/web/src/lib/` | pure helpers and browser adapters; `uiStateReport.ts` exports typed portable state, `uiCommandDispatch.ts` owns the eight publication-only commands, and `uiLayoutApply.ts` + `uiLayoutApplyCore.ts` own exact-target acknowledged apply; `terminalCellGeometry.ts` is the ONE pixels→cols/rows measurement, shared by the live view claim and the pre-spawn size hint; agent seen tokens, notification timers, and cross-tab claims pin exact epoch/occupant revisions; `globalContentSearchController.ts`/`globalContentSearchResults.ts`/`globalContentSearchRuntime.ts` own bounded search and `terminalFindIntent.ts`/`terminalFindHandoff.ts` rerun matches against the current grid epoch (`cellRenderer.ts`, `cellRow.ts`, `terminalInputController.ts`, `deckSwipe.ts`, prefs, diag); `localWorkerDiscovery.ts` owns the one-shot probe for a worker door on this browser's machine while `localBootstrap.ts` stays the served-BY-a-worker fact `connect.ts` routes RPCs off; `predictiveEcho.ts` plus `predictiveEchoExpiry.ts`/`predictiveEchoGrid.ts`/`predictiveEchoOverlay.ts`/`predictiveEchoPaint.ts` own local keystroke prediction, its expiry and its paint; `terminalStartupProgress.ts` owns the monotone opening-terminal stage/percent series; `terminalInputStatus.ts` phrases a send's outcome; `browseEntries.ts`, `browseErrorMessage.ts` and `folderNameValidation.ts` back the folder picker; `deckTabBadge.ts` counts the compact deck's terminals | JSX or terminal stream owner |
| `apps/web/src/auth/` | web-key/IndexedDB, `fragment-credential.ts` (`#pair=<token>`, the only URL credential kind), pairing and tab identity | RPC plumbing (`apps/web/src/connect.ts`) or UI |
| `apps/web/src/styles/` | global stylesheets imported once by `main.tsx`; `theme-vars.css` owns canonical theme tokens and aliases; `components/Settings/md/tokens.css` owns shared settings primitives; `sidebar.css` owns terminal `.wterm` and legacy drawer rules; `workbench-shell.css` owns desktop shell and workbench-mounted Settings presentation; `workbench-sidebar.css` and `workbench-tabs.css` own sidebar and tab/deck presentation respectively | component-local one-offs |
| `apps/web/tests/` | recursive `*.test.ts` Bun suites, including the root `*.dom.test.ts` fake-DOM suites | browser-real assertions |
| `apps/web/tests/helpers/` | shared non-suite fixtures: `cellRendererFakeDom.ts`, `terminalStreamFixture.ts` | test registration |
| `apps/web/public/` | static assets copied verbatim: fonts, icons, `manifest.webmanifest`, `sw-push.js`, `whatsnew.json`, pinned `wterm-roost.wasm` | generated build output |

## Canonical workbench

`AppShell` owns the persistent desktop title bar, activity rail, primary sidebar,
editor/deck, and status bar. `MainPane` remains the single route-array owner of
the warm `TerminalDeck`; navigation across session, file, and search routes
does not remount its terminal renderers. Desktop tab geometry reads the resolved
`--workbench-tab-strip-height` once during deck sizing, while compact mode keeps
its 48px mobile strip.

The compact shell removes desktop rails, opens the existing drawer for
navigation, and keeps route context in one `MobileTopBar`. Settings uses the
shared list-row primitives for its mobile list/detail/back flow.

Real-surface proof lives in `smoke/terminal/workbench-shell.spec.ts` and the
targeted terminal Playwright tier. Manual visual captures use
`bun smoke/terminal/live-stack.ts --pair`; the `/design` route is the static
token and primitive reference, not a substitute for a real terminal run.

`/search` is a dashboard-local metadata, terminal-content, and agent-attention
surface. Its default scope filters the same scalar metadata projection used by
the sidebar and palette while a debounced controller searches authorized open
sessions' retained terminal rows. Content pages are explicitly continued with
`Load more`; worker/session partials remain visible instead of becoming false
empty results. Selecting a content match navigates by the current projection's
session href and asks that pane's find controller to rerun the literal query
against its current grid epoch before revealing anything. `scope=attention`
retains current blocked rows and unseen completions without issuing content
search RPCs; opening a completed session acknowledges it. Search never inspects
agent transcripts. Attachment upload/download through `TransferStack` remains
supported.

Browser-local layouts remain in `paneLayoutStore`, which keeps the active
runtime tree and private pane/split UUIDs under `roost.paneLayout.v1`.
`@roost/shared/layout-document` is the strict versioned boundary for
folder-scoped `UiReportState`; reports replace runtime IDs with deterministic
preorder leaf/slot keys and send typed protobuf rather than embedded runtime
JSON. The acknowledged remote exception targets one page's current Sync
socket, while that page derives the URL-active folder and canonical live
membership before calling `applyLayoutDocument`. No coordinator record,
storage event, or open peer tab owns or live-folds another page's layout.

## Invariants

Break one of these and you get back the history-corruption class this repo keeps re-fixing.

- **The browser never parses VT and never re-reflows history.** The worker owns the authoritative
  grid; `apps/web/src/lib/cellRenderer.ts` paints immutable cell rows at the worker's grid width and
  letterboxes surplus pane width. There is no client-side re-parse at a new width, no mirrored grid,
  no output reparse. Raw PTY bytes never enter the browser Sync socket.
  The same ownership governs history CONTENT: the renderer never infers WHICH rows scrolled off the
  viewport and never paints a history row it was not handed — only a frame's own `scrollbackRows` /
  `scrollbackAppend` and an epoch-addressed `SessionsGetScrollbackCells` page reach the history sheet.
- **`apps/web/src/lib/cellRenderer.ts` is ONE class and is never split.**
  `CellGridRenderer` methods share private per-frame state (`frame`,
  reader-intent holds,
  the owned-scroll epoch); that encapsulation is the invariant. It carries
  `// ─── frame application ───`, `// ─── reader-intent holds ───` and `// ─── scroll ownership ───`
  banners at the method-group boundaries — navigate by those, do not extract past them. It is
  baselined in `scripts/file-size-baseline.json`.
- **Only `_pinToBottom(shouldPin)` may assign `scrollTop`,** through the single conditional writer
  `_writeScrollTop`, and only when a pre-mutation FOLLOW-BAND check was true
  (`_atBottomOrOwnedPlacement` → `followsBottom()` → `followsScrollBottom`, two rows of slack around
  the exact clamp, `BOTTOM_FOLLOW_SLACK_ROWS` in `cellRendererPresentation.ts`). A reader inside the
  band is riding the tail, so the pin keeps it there; one appended row would otherwise drift it out.
  `atBottom()` itself stays EXACT and keeps its meaning. Nothing else in the
  app writes terminal scroll position. Scrollback rows are append-only and immutable; every
  `content-visibility` block gets an exact pixel placeholder (`blockPlaceholder`) so a revealed block
  cannot move the scroll maximum out from under a pinned pane. An unpainted reserved gap
  (`_extendScrollbackGap`) plus demand backfill is the ONLY representation the browser has of history
  it has not been given; a viewport-only checkpoint reserves that depth and paints nothing into it.
  `scrollbackBackfill.ts` fills those gaps AHEAD of the reader: the trigger window is widened upward
  by `BACKFILL_AHEAD_ROWS`, a scroll page is anchored at the newest missing row and extends
  `BACKFILL_FETCH_ROWS` older, and exactly one wave is in flight — a scroll raised mid-wave is
  coalesced, and every settle re-derives the demand from live scroll state.
- **A reader park must be exitable by an event the pane can still deliver.** A park whose whole
  state is a scroll position (`native_scroll`, `wheel`, `touch` — `isPositionOnlyReaderReason` in
  `apps/web/src/lib/cellRendererPresentation.ts`) resumes from `noteBoxResize()` when the reader sat
  at the OLD box's bottom; `selection` and `find` own an anchor and keep their park. Any park
  resumes when the post-resize box leaves no scroll range (`scrollHeight <= clientHeight`), because
  a box that cannot scroll can never fire another scroll event and `handleScroll()`'s bottom resume
  becomes unreachable — the pane would be frozen forever with `atBottom()` already true. Paint holds
  (`RENDERER_HOLD_SELECTION`, `RENDERER_HOLD_LINK`) are level-derived from the live document, not
  latched on an edge: `cell-terminal-interactions.ts` re-runs `syncNativeSelectionHold()` when it
  re-attaches its global listeners, and every container pointer event re-derives the link hold's
  modifier level in BOTH directions (`components/terminal-links.ts`, with a TOTAL predicate — an
  event carrying no modifier fields reads as "not held"), because a hold must never outlive the
  level that justifies it. Releasing the last hold resumes a `selection` park, and any park whose
  box has lost its scroll range. `_resumeLive` refuses a held pane BEFORE mutating reader state, so
  a frozen pane reports its real intent, reason and `reconcile_block_reason` instead of a lying
  `live`/`null`. Dismissing the find bar ends the find reading INTERVAL through
  `endFindReading()` (reason `find` → `native_scroll`, no scroll write, no pin, no frame applied),
  so a dismissed park is an ordinary scroll park instead of an anchor every recovery refuses.
- **The band decides who parks; the settle decides when a park ends by itself.** `handleScroll()`
  parks a live reader only when it is OUTSIDE the follow band, so sub-row jitter, a fractional
  clamp and a flick that lands a row short cannot freeze the pane, while one wheel notch (~100px)
  still parks it. A park that came to REST inside the band resumes through `settleFollowBand()`,
  which `cell-terminal-renderer.ts` arms `BOTTOM_FOLLOW_SETTLE_MS` after the last scroll event —
  from its scroll listener (`restartFollowSettle`) AND, because the wheel/touch classifier can park
  a reader after that listener has run for the last time, from frame arrival
  (`_settleBottomPark()` → the injected `requestFollowBandSettle` → `ensureFollowSettle`, which
  opens a window only when none is pending so a frame stream cannot defer its own resume). Never
  from `handleScroll()` synchronously and never straight from a frame, because a `scrollTop` write
  mid-gesture cancels the scroll animation Chromium is still running for the reader. Releasing the
  last paint hold also resumes a band-following position-only park, because the hold swallowed the
  one scroll event that proved the reader came back. `_settleBottomPark()`'s rAF settle still
  demands `atBottom()` exactly — the band never widens the clamp paths.
- **`CellTerminal` renders inside the `<For>` deck, never a `<Show>`.** `src/components/TerminalDeck.tsx` feeds
  `<For each={mountedSessionIds()}>` primitive session ids (not `Session` objects) so a root snapshot
  that replaces a same-id object cannot tear down a warm renderer; a remount loses scrollback. Guard:
  lint rule `L11: CellTerminal must render inside <For> deck, never <Show> (remount on nav loses
  scrollback)` in `scripts/lint-roost.ts`, pinned against `apps/web/src/components/MainPane.tsx`.
- **Single root store.** `apps/web/src/store/root.ts` is the only `createStore<RootState>`. Components
  subscribe to `createMemo` selectors in `apps/web/src/store/selectors.ts` and write only through the
  named functions in `apps/web/src/store/mutations.ts`. New UI adds a selector and a JSX line; it does
  not add a store. `apps/web/src/store/projector.ts` folds `SessionEvent` with the same `foldEvent`
  coord uses (`@roost/shared/wire`), so SPA and coord projections agree by construction.
- **Portable layout apply is one validated browser-local commit.** The shared
  V1 parser rejects unknown keys/versions, invalid graph references, and
  excessive identifiers, depth, nodes, slots, or bindings before recursion.
  `liveSessionIdsForFolder()` is the only folder membership/order selector used
  by the deck, reporter, and command adapters; the optimistic spawn projection
  removes client-only identities for reports and exposes their presence so
  acknowledged apply can fail closed. Reporting contains stable leaf/slot keys
  only; runtime pane/split UUIDs, focused pane IDs, and redundant
  visible-session lists never cross the wire. The adapter validates and
  materializes entirely in locals before `commitLayout()` exactly once;
  rejection cannot create a pane signal, subscriber call, persistence timer, or
  localStorage write. A leaf holding no session collapses into its sibling
  before that commit, so no pane commits without a tab strip to close it; a
  single empty root leaf is the only legal empty result. Acknowledged apply
  stays exact and rejects any document whose bindings are no longer live.
  Legacy runtime ratios normalize into the shared bounds before rendering or
  reporting. Desktop Arrange exposes presets only. Compact rendering projects
  the live URL session (or first occupied leaf) without changing the preserved
  desktop focus/topology.
- **Acknowledged apply never transfers layout ownership.** `uiLayoutApplyCore.ts`
  accepts only a frame whose nonempty tab, socket, and correlation match the
  page's current identity and Sync-v2 generation. It rechecks that the URL names
  an open, coordinator-admitted session in the canonical live set and rejects
  the whole operation when the active session or any folder sibling is a
  pending/tombstoned optimistic identity. It decodes through the shared adapter
  and invokes `applyLayoutDocument` once, then clears spotlight, attempts
  focused-selection navigation, and attempts to send `applied` on the same
  socket in a `finally`; `applied` proves the commit, not navigation completion,
  and a post-commit throw cannot become retry-inviting rejection. Each exact
  local decision emits one bounded correlation/outcome diagnostic. Invalid
  current membership or document returns a stable sanitized `rejected` reason
  without mutation. Wrong/stale frames are ignored; a result unavailable after
  socket replacement remains coordinator-side `target_gone` ambiguity and is
  never retried. Open pages still ignore one another's layout storage events.
- **UI reports wait for authoritative session identity.** A client-only
  optimistic `/s/:id` is blanked and omitted from layout bindings. Successful
  admission schedules a fresh report even after the initial debounce elapsed.
  Failed/aborted identities enter a bounded tombstone set so late rows remain
  excluded while asynchronous cleanup completes. An unchanged cold session
  route schedules another report when hydration resolves it, rather than
  waiting for the minute heartbeat.
- **Observed-agent status is occupant-fenced.** Identified Sync frames compare
  `status_epoch` and `occupant_id` only by equality; `source` is mutable
  provenance. Replaced occupants and epochs stay retired, and seen state,
  notification timers, cross-tab claims, badges, and attention rows use the
  exact occupant revision. Identityless rolling-deployment frames remain
  displayable but cannot supersede an identified occupant. An `occupant_exited`
  row is kept only while this profile still owes it a completion; once
  acknowledged — here or in another tab sharing the profile — it is retired
  from the projection, because a dead agent with nothing unseen has no surface.
- **The Sync generation set is exact.** The browser accepts terminal, workers, workspaces, tasks,
  MCP, pair, and audit generations, with audit as the only lazy domain. Missing or extra domains are
  a protocol mismatch that requires the current SPA to reload; there are no tombstone domains or
  compatibility subscriptions.
- **Only visible panes publish active terminal views.** Input goes through
  `sendTerminalInput` in `apps/web/src/ws/sync-outbound.ts`; every batch resolves
  accepted/rejected/ambiguous and is never silently retried. `src/store/terminal-stream.ts`
  owns stable `view_id` handles, revisions, Sync-generation replay and one
  canonical viewport replica per session. `CellTerminal.tsx` composes the pane;
  `src/components/cell-terminal-viewport.ts` alone publishes active/inactive
  view geometry, while `src/components/cell-terminal-renderer.ts` attaches the
  renderer/stream. Hidden panes receive no cells, but detach or tab switching
  cannot delete the session replica; reactivation receives a complete baseline
  before deltas.
- **Terminal incident capture is opt-in and consent-gated.** Ordinary terminals
  keep only their existing content-free diagnostics.
  `apps/web/src/components/TerminalContextMenu.tsx` carries `Start
  terminal debugging` (`ctx-debug-start`), `Capture terminal diagnostic`
  (`ctx-capture-diagnostics`) and `Stop terminal debugging` (`ctx-debug-stop`) in
  BOTH the floating and the compact-sheet branch, with the lease phase on its own
  non-interactive `ctx-capture-state-row` (`StatusDot` + label ramp) rather than
  inside the item that is disabled while recording.
  `TerminalCaptureStateRow.tsx` renders that row,
  `TerminalCaptureConsentDialog.tsx` owns the confirmation and
  `terminalCaptureMenuController.ts` owns the lease calls: starting, and any manual capture
  without an acknowledged lease, requires a confirmation stating that terminal
  text and raw output may contain secrets and are retained at most
  `TERMINAL_CAPTURE_LIMITS.retentionMs`; the consent grants a lease for that
  session only, and an expired lease must be started again rather than renewed
  after a reload. The controller calls `freezeTerminalCaptureEvidence` BEFORE the
  dialog opens (dismissal and focus change reader holds) and discards the token on
  cancel, so nothing leaves the browser before consent. Confirming START arms the
  lease and then discards that pre-arm freeze — arming writes no bundle; only a
  CAPTURE sends `captureTerminalIncidentFrozen`. Saved bundles come back through the existing
  authenticated file path only: `apps/web/src/lib/terminalCaptureDownload.ts`
  composes `workerFileHref(workerFp, path)` and hands it to
  `downloadWorkerFileByHref`, so a capture is never an unauthenticated URL and
  never an ordinary attachment. An automatic capture reports "Terminal diagnostic
  captured" with a Download action, a worker-detected incident reports "Worker
  detected a terminal incident", and a manual capture names and starts that
  download immediately. `apps/web/src/lib/terminalSnapshotFacade.ts` keeps
  `window.__roostTerminalSnapshot` content-free and re-exports
  `startTerminalCapture` / `captureTerminalIncident` / `stopTerminalCapture`; no
  generic command evaluator is installed on `window`.
- **Text composition and raw input are distinct contracts.** The browser
  composer imports `buildPtyPayload`, newline normalization, bracketed-paste
  framing, and CR from `@roost/shared/terminal-input`; when bracketed paste is
  active, that owner strips ESC from the text before wrapping it. The worker's
  guarded prompt uses the same owner. Sync input and public `SessionsInput`
  still carry caller-encoded raw bytes and never acquire a status fence,
  transformation, implicit Enter, or retry.
- **One notification dock owns bottom overlay placement.**
  `apps/web/src/components/NotificationDock.tsx` is the only SPA surface that decides where a
  transient notification sits: toasts (`ToastStack.tsx` + `ToastCard.tsx`), undo snackbars,
  transfers and pair requests are its flex children, so two of them cannot claim the same rect,
  and `apps/web/src/lib/notificationDockLift.ts` is the single formula that clears either the
  compact viewport composer plus soft keyboard or the desktop status bar plus in-pane composer
  row. A new transient surface becomes a dock child — never a new `position: fixed` corner with
  its own `z-index`.
- **Design system: no raw values in components.** No hex, `rgb()`, or px font-size outside the
  token-declaration files — `apps/web/src/styles/theme-vars.css`,
  `apps/web/src/styles/voice-input.css`,
  `apps/web/src/components/Settings/md/tokens.css`,
  `apps/web/src/components/Settings/md/icon.css` — plus the two palette sources
  `apps/web/src/lib/themes.ts` and `apps/web/src/lib/agents.ts`. Reference tokens via `var(--…)` with
  **no** fallback (tokens are always defined), and compose from
  `apps/web/src/components/Settings/md/primitives.tsx`. Selected state is
  `--md-sys-color-secondary-container`, not the removed `--bg-selected` / `--border-selected` tints.
  Ratcheted by `bun run lint` against `scripts/design-raw-baseline.json`; re-snapshot with
  `bun scripts/lint-roost.ts --update-design-baseline`.
- Other live lint guards on this app: `.wterm` must keep `overflow-y: auto` in
  `apps/web/src/styles/sidebar.css`; never force `_doRender()` inside the `CellTerminal` byte handler
  (regressed three times); never read `props.*` inside `onCleanup`;
  `setRootStore("key", (prev) => newRecord)` on a `Record` silently no-ops; sidebar `data-selected` is
  URL-driven; `addToast` kind is `ok | warn | err`.

## How to test it

- `bun run test:unit` — the hermetic tier. Runs `scripts/test-worker.ts`, then `bun test --isolate
  --timeout 30000` over `apps/web/tests/` and `apps/web/src/` alongside the shared, coord and CLI
  suites. `--isolate` is load-bearing: files here install fake DOM globals and call `mock.module`,
  both of which poison every later file that would otherwise share the process.
- `bun run test:terminal` — the Playwright browser tier, and the only tier that proves paint. It
  builds this app (`vite build`), regenerates the embeds (`scripts/gen-embed.ts`), runs pass 1
  `--project=chromium-desktop` (plus `--project=webkit-iphone` on darwin)
  across `smoke/terminal/**/*.spec.ts`. Repo-root
  `playwright.config.ts` derives pass-1 workers from available CPUs, capped at
  four. Pass 2 uses `--project=chromium-serial --workers=1` for `@serial` perf
  cases, then the runner restores embed stubs
  (`scripts/gen-embed.ts --stub`) in a `finally`.
- **Fake DOM, not jsdom.** The 23 `apps/web/tests/*.dom.test.ts` suites use a
  hand-rolled fake DOM; this repo runs no jsdom or happy-dom. Solid resolves to
  its SSR build under `bun test`, so a DOM emulator buys nothing and the fake
  asserts exactly what the code touches. Shared renderer fixtures are in
  `apps/web/tests/helpers/cellRendererFakeDom.ts`; the
  `cellRenderer.*.dom.test.ts` files consume them.
  Do not introduce a DOM emulator to make a test easier.
- `bun run --cwd apps/web typecheck` type-checks this workspace; CI type-checks the whole tree with
  `bun x tsgo -p tsconfig.base.json --noEmit`.
- Renderer correctness beyond the deterministic tiers is proven inside `bun run test:terminal`:
  `runRenderStress` (`src/lib/smokeHarness.ts`) drives the resize / tab-switch / multi-viewer loops in
  `smoke/terminal/terminal-render*.spec.ts` and fails on duplicated, lost, changed or mis-ordered
  markers — the corruption class the unit tier cannot see.