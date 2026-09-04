# apps/web — the Solid SPA

The browser client. One Solid 1.x app on plain Vite (`bun x vite` on :5174 in dev, `bun x vite build`
into `apps/web/dist/` which coord serves in production). It paints terminal cells the worker already
rendered; it does not run a terminal core.

Path references are relative to `apps/web/` unless they start at the repo root (`apps/…`, `scripts/…`, `smoke/…`, `docs/…`).

## Entry point

`apps/web/index.html` loads `apps/web/src/main.tsx`. In order, before any component renders:

1. `applyTheme(loadTheme())` (`apps/web/src/lib/theme.ts`) — sets `data-theme` so first paint is not
   flash-of-wrong-theme.
2. `installSignalShip()` + `installSpaDiag()` (`apps/web/src/lib/diag.ts`) — Tier-1 `signal` is always
   on and ships anomalies to coord's `*.err.log`; Tier-2 `diag` firehose is gated on
   `localStorage.roostDiag`. Installed before render so `spa.uncaught` catches setup throws.
3. `window` `error` / `unhandledrejection` / `vite:preloadError` handlers — chunk-mismatch recovery
   after a redeploy, loop-guarded through `sessionStorage` by `apps/web/src/lib/chunkError.ts`.
4. `applyTermFontSize()` (`apps/web/src/lib/terminalFontPref.ts`) — pushes persisted terminal zoom
   onto the document *before* the first pane measures its cell box, or that pane claims a viewport
   sized for 14px and immediately re-claims.
5. `render(() => <App />, #app)`, then `installLeakWatch()` and `void loadAgentConfig()`.

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

One row per directory in this workspace. Within a row, a bare file name lives in that
row's directory; any ref with a path prefix follows the convention at the top of this file.

| Directory | Owns | Must not own |
| --- | --- | --- |
| `apps/web/src/` (root files) | `main.tsx` (boot), `App.tsx` (router + overlay shell), `routes.ts` (URL table), `connect.ts` (Connect-RPC client: binary protobuf + JWT interceptor), the two `*.d.ts` ambient decls | anything feature-shaped; every screen lives under `apps/web/src/components/` |
| `apps/web/src/components/` | screens, panes, dialogs, and the terminal surface (`CellTerminal.tsx`, `TerminalDeck.tsx`, `MainPane.tsx`) | direct store writes (use `apps/web/src/store/mutations.ts`), wire framing, persistence |
| `apps/web/src/components/layout/` | `AppShell.tsx` (sidebar + main 2-pane shell, mobile drawer) and `MobileTopBar.tsx` | route-specific content; it renders the route slot |
| `apps/web/src/components/sidebar/` | machine / folder / session lists, sidebar search, row context menus, `ViewersChip.tsx` | per-view stores — selection and filtering derive from the URL and `rootStore` |
| `apps/web/src/components/Settings/` | the settings panes + `SettingsRoot.tsx` + `CoordinatorMoveDialog.tsx`; MCP remains under Agents, and the workers pane is `MachinesPane.tsx` (there is no `WorkersPane`) | raw CSS values; panes compose the primitives in `apps/web/src/components/Settings/md/` |
| `apps/web/src/components/Settings/md/` | the M3 primitive set (one component per file, re-exported by `primitives.tsx`) plus `tokens.css` / `icon.css` — a token *declaration* site | app state or data fetching; primitives are presentational |
| `apps/web/src/store/` | the single reactive-global-state tier: `root.ts` + `selectors.ts` + `mutations.ts`, the projector, the Sync client (`sync.ts` and its `sync-*.ts` leaves), the per-session terminal replica/view registry (`terminal-stream.ts`), pane-layout tiling, and **every** UI-state store (`uiStore.ts`, `toastStore.ts`, `renameDialog.ts`, `queueTaskDialog.ts`) | JSX, and module-level `let _ws` / `let _reconnectTimer` / `let _backoffMs` (lint-guarded) |
| `apps/web/src/ws/` | the **outbound** half of the Sync v2 socket: PTY input plus terminal-view command transport (`sync-outbound.ts`) and smoke backdoor hooks | the socket, inbound dispatch, terminal membership, or terminal continuity — those live in `apps/web/src/store/` |
| `apps/web/src/lib/` | pure helpers, DOM controllers, and browser-API adapters (`cellRenderer.ts`, `cellRow.ts`, `terminalInputController.ts`, `ptyPaste.ts`, `deckSwipe.ts`, prefs, diag) | JSX or terminal stream ownership — this directory holds zero `.tsx` files, which is why `src/components/UiBridge.tsx` lives in `apps/web/src/components/` |
| `apps/web/src/auth/` | credential material: ed25519 WebCrypto key + IndexedDB (`web-key.ts`), pair-token redemption, per-tab identity, coordinator relocation | RPC plumbing (`apps/web/src/connect.ts`) and any UI |
| `apps/web/src/styles/` | the six eagerly-imported global stylesheets; `theme-vars.css` is the alias graph the theme engine writes into, `sidebar.css` carries the `.wterm` terminal shell | component-local one-offs; scoped styles stay inline in the component |
| `apps/web/tests/` | the hermetic Bun tier (~80 files), including 10 hand-rolled fake-DOM suites and the shared shim `apps/web/tests/helpers/cellRendererFakeDom.ts` | browser-real assertions — those are Playwright specs in `smoke/terminal/` |
| `apps/web/public/` | static assets copied verbatim: fonts, icons, `manifest.webmanifest`, `sw-push.js` (push renderer), `whatsnew.json`, the pinned `wterm-roost.wasm` | anything generated by the build |

## Invariants

Break one of these and you get back the history-corruption class this repo keeps re-fixing.

- **The browser never parses VT and never re-reflows history.** The worker owns the authoritative
  grid; `apps/web/src/lib/cellRenderer.ts` paints immutable cell rows at the worker's grid width and
  letterboxes surplus pane width. There is no client-side re-parse at a new width, no mirrored grid,
  no output reparse. Raw PTY bytes never enter the browser Sync socket.
- **`apps/web/src/lib/cellRenderer.ts` is ONE class and is never split.** `CellGridRenderer` is
  ~1296 lines and ~53 methods that share private per-frame state (`frame`, reader-intent holds,
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
- **The Sync generation set is exact.** The browser accepts terminal, workers, workspaces, tasks,
  MCP, pair, and audit generations, with audit as the only lazy domain. Missing or extra domains are
  a protocol mismatch that requires the current SPA to reload; there are no tombstone domains or
  compatibility subscriptions.
- **Only visible panes publish active terminal views.** Input goes through
  `sendTerminalInput` in `apps/web/src/ws/sync-outbound.ts`; every batch resolves
  accepted/rejected/ambiguous and is never silently retried. `src/store/terminal-stream.ts`
  owns stable `view_id` handles, revisions, Sync-generation replay and one
  canonical viewport replica per session. `CellTerminal` only calls
  `view.setViewport(...)` / `view.setInactive()` and attaches a renderer. A
  hidden renderer receives no cells, but detach or tab switching cannot delete
  the session replica; reactivation receives a complete baseline before deltas.
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
  `--project=chromium-desktop` (plus `--project=webkit-iphone` on darwin) across the 23 spec files in
  `smoke/terminal/` at `playwright.config.ts`'s `workers: 4`, then pass 2
  `--project=chromium-serial --workers=1` for the `@serial` perf cases, and restores the embed stubs
  (`scripts/gen-embed.ts --stub`) in a `finally`.
- **Fake DOM, not jsdom.** `apps/web/tests/*.dom.test.ts` use a hand-rolled fake DOM and this repo
  runs no jsdom or happy-dom, by design: Solid resolves to its **SSR build** under `bun test`, so a
  DOM emulator buys nothing and the fake asserts exactly what the code touches (node identity, one
  conditional scroll writer). The shared shim + frame builders are
  `apps/web/tests/helpers/cellRendererFakeDom.ts`; the `cellRenderer.*.dom.test.ts` files consume it.
  Do not introduce a DOM emulator to make a test easier.
- `bun run --cwd apps/web typecheck` type-checks this workspace; CI type-checks the whole tree with
  `bun x tsgo -p tsconfig.base.json --noEmit`.
- Renderer correctness beyond the deterministic tiers is proven inside `bun run test:terminal`:
  `runRenderStress` (`src/lib/smokeHarness.ts`) drives the resize / tab-switch / multi-viewer loops in
  `smoke/terminal/terminal-render*.spec.ts` and fails on duplicated, lost, changed or mis-ordered
  markers — the corruption class the unit tier cannot see.