# apps/web — the Solid SPA

The browser client. One Solid 1.x app on plain Vite (`bun x vite` on :5174 in dev, `bun x vite build`
into `apps/web/dist/` which coord serves in production). It paints terminal cells the worker already
rendered; it does not run a terminal core.

Path references are relative to `apps/web/` unless they start at the repo root (`apps/…`, `scripts/…`, `smoke/…`, `docs/…`).

## Entry point

`apps/web/index.html` loads `apps/web/src/entry.ts`, a dependency-minimal
security boundary. Startup order:

1. `captureAndScrubFragmentCredential()` synchronously removes URL-carried
   credentials before the SPA module graph loads.
2. `entry.ts` dynamically imports `apps/web/src/main.tsx`; it installs the
   tenant-route switch listener and awaits
   `completePendingTenantRouteSwitch()` before diagnostics or transport.
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
| `apps/web/src/components/` | screens/dialogs; `GlobalSearchPage.tsx` composes metadata/attention with `GlobalSearchContentResults.tsx`; `ArrangeMenu.tsx` exposes pane presets plus local layout transfer and `LayoutDocumentDialog.tsx` owns explicit import preview/apply; `CellTerminal.tsx` composes `cell-terminal-types.ts`, `cell-terminal-runtime.ts`, `cell-terminal-input.ts`, `cell-terminal-presentation.ts`, `cell-terminal-viewport.ts`, `cell-terminal-renderer.ts`, `cell-terminal-interactions.ts`, and `cell-terminal-lifecycle.ts`; `TerminalDeck.tsx` owns persistent keyed mounting; `ManagedRouteGate.tsx` and `ManagedLogin.tsx`/`ManagedSignup.tsx` own managed gates/routes | direct store writes, wire framing, persistence |
| `apps/web/src/components/layout/` | `AppShell.tsx` (sidebar + route slot), `MobileTopBar.tsx`, `DashboardScopeSelector.tsx` (server-confirmed organization/dashboard selection) | route-specific content |
| `apps/web/src/components/sidebar/` | machine / folder / session lists, sidebar search, row context menus, `ViewersChip.tsx` | per-view stores — selection and filtering derive from the URL and `rootStore` |
| `apps/web/src/components/Settings/` | settings shell/panes; `OrganizationPane.tsx` and `DashboardPane.tsx` project confirmed scope, `MachinesPane.tsx` owns workers, `settingsNavigation.ts` hides self-hosted-only scope controls in managed mode | raw CSS values; panes compose `apps/web/src/components/Settings/md/` |
| `apps/web/src/components/Settings/md/` | one-component-per-file M3 primitives re-exported by `primitives.tsx`; `tokens.css` consumes canonical theme variables and `icon.css` styles icons | app state, data fetching, or token declarations |
| `apps/web/src/store/` | single reactive state: `root.ts`, selectors/mutations/projector, Sync leaves, terminal replica/view leaves, pane/UI stores; `paneLayoutDocument.ts` is the portable-document adapter over the browser-local pane store — strict for every apply, degrading only for the human-confirmed import preview; `agent-status.ts` owns epoch/occupant admission and retired-identity fencing; `dashboard-selection.ts` owns access bootstrap, remembered hints, generation-fenced resources, and atomic scope cutover | JSX or module-global socket/reconnect state |
| `apps/web/src/ws/` | the **outbound** half of Sync v2: PTY input, terminal-view commands (`sync-outbound.ts`), smoke hooks | socket, inbound dispatch, membership, or continuity |
| `apps/web/src/lib/` | pure helpers and browser adapters; `layoutDocumentControls.ts` + `layoutDocumentFile.ts` own local transfer, `uiStateReport.ts` exports typed portable state, `uiCommandDispatch.ts` owns the eight publication-only commands, and `uiLayoutApply.ts` + `uiLayoutApplyCore.ts` own exact-target acknowledged apply; agent seen tokens, notification timers, and cross-tab claims pin exact epoch/occupant revisions; `globalContentSearchController.ts`/`globalContentSearchResults.ts`/`globalContentSearchRuntime.ts` own bounded search and `terminalFindIntent.ts`/`terminalFindHandoff.ts` rerun matches against the current grid epoch (`cellRenderer.ts`, `cellRow.ts`, `terminalInputController.ts`, `deckSwipe.ts`, prefs, diag) | JSX or terminal stream ownership; this directory has zero `.tsx` files |
| `apps/web/src/auth/` | web-key/IndexedDB, fragment credentials, pairing/tab identity/relocation; `tenant-routing.ts`, `managed-routes.ts`, `managed-auth-gateway.ts`, `managed-login.ts`, `managed-account.ts`, `managed-credentials.ts`, and `managed-logout.ts` own managed policy/transitions | RPC plumbing (`apps/web/src/connect.ts`) or UI |
| `apps/web/src/styles/` | six global stylesheets imported by `main.tsx`; `theme-vars.css` is the canonical token/alias graph, `sidebar.css` owns `.wterm` shell rules | component-local one-offs |
| `apps/web/tests/` | recursive `*.test.ts` Bun suites, including the root `*.dom.test.ts` fake-DOM suites | browser-real assertions |
| `apps/web/tests/helpers/` | shared non-suite fixtures: `cellRendererFakeDom.ts`, `terminalStreamFixture.ts` | test registration |
| `apps/web/public/` | static assets copied verbatim: fonts, icons, `manifest.webmanifest`, `sw-push.js`, `whatsnew.json`, pinned `wterm-roost.wasm` | generated build output |

Managed per-account isolation and these auth/dashboard modules are qualified,
but the managed service is not publicly launched in v0.5.0. Accounts are
operator-created; open signup and production managed image publication are off.

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
agent transcripts. Cross-worker transfer remains a beta placeholder: its item
opens an explanatory dialog without issuing a transfer RPC. Attachment
upload/download through `TransferStack` remains supported.

Portable layouts remain browser-owned. `paneLayoutStore` keeps the active
runtime tree and private pane/split UUIDs under `roost.paneLayout.v1`;
`@roost/shared/layout-document` is the strict versioned boundary. Export and
folder-scoped `UiReportState` documents replace runtime IDs with deterministic
preorder leaf/slot keys; reports send typed protobuf rather than embedded
runtime JSON, and off-folder routes omit the document.
Local import previews before mutation. The acknowledged remote exception
targets one page's current Sync socket, but that page still derives the
URL-active folder and canonical live membership and calls the same
`applyLayoutDocument`. No coordinator record, storage event, or open peer tab
owns or live-folds another page's layout.

## Invariants

Break one of these and you get back the history-corruption class this repo keeps re-fixing.

- **The browser never parses VT and never re-reflows history.** The worker owns the authoritative
  grid; `apps/web/src/lib/cellRenderer.ts` paints immutable cell rows at the worker's grid width and
  letterboxes surplus pane width. There is no client-side re-parse at a new width, no mirrored grid,
  no output reparse. Raw PTY bytes never enter the browser Sync socket.
- **`apps/web/src/lib/cellRenderer.ts` is ONE class and is never split.**
  `CellGridRenderer` methods share private per-frame state (`frame`,
  reader-intent holds,
  the owned-scroll epoch); that encapsulation is the invariant. It carries
  `// ─── frame application ───`, `// ─── reader-intent holds ───` and `// ─── scroll ownership ───`
  banners at the method-group boundaries — navigate by those, do not extract past them. It is
  baselined in `scripts/file-size-baseline.json`.
- **Only `_pinToBottom(wasAtBottom)` may assign `scrollTop`,** through the single conditional writer
  `_writeScrollTop`, and only when an exact pre-mutation bottom check was true. Nothing else in the
  app writes terminal scroll position. Scrollback rows are append-only and immutable; every
  `content-visibility` block gets an exact pixel placeholder (`blockPlaceholder`) so a revealed block
  cannot move the scroll maximum out from under a pinned pane.
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
  by the deck, reporter, command adapters, and import controls; the optimistic
  spawn projection removes client-only identities for reports and exposes their
  presence so acknowledged apply can fail closed. Export and reporting contain
  stable leaf/slot keys only; runtime pane/split UUIDs, focused pane IDs, and
  redundant visible-session lists never cross the wire.
  The adapter validates and materializes entirely in locals before
  `commitLayout()` exactly once; rejection cannot create a pane signal,
  subscriber call, persistence timer, or localStorage write. A leaf holding no
  session collapses into its sibling before that commit, so no pane commits
  without a tab strip to close it; a single empty root leaf is the only legal
  empty result. A human-confirmed import drops bindings whose session is no
  longer live, collapses the panes they emptied, and reports that count in the
  confirm dialog; acknowledged apply stays exact and rejects the same document.
  Legacy runtime ratios normalize into the shared bounds before rendering or
  export. Desktop Arrange and the compact workspace-sheet overflow expose the
  same controls, including for one-session folders. Compact rendering projects
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
- **Text composition and raw input are distinct contracts.** The browser
  composer imports `buildPtyPayload`, newline normalization, bracketed-paste
  framing, and CR from `@roost/shared/terminal-input`; when bracketed paste is
  active, that owner strips ESC from the text before wrapping it. The worker's
  guarded prompt uses the same owner. Sync input and public `SessionsInput`
  still carry caller-encoded raw bytes and never acquire a status fence,
  transformation, implicit Enter, or retry.
- **Design system: no raw values in components.** No hex, `rgb()`, or px font-size outside the
  token-declaration files — `apps/web/src/styles/theme-vars.css`,
  `apps/web/src/styles/syntax-vars.css`, `apps/web/src/styles/voice-input.css`,
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
- **Fake DOM, not jsdom.** The 19 `apps/web/tests/*.dom.test.ts` suites use a
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