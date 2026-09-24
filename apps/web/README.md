<!-- AUDIENCE: claude -->
<!-- Layered web map: client helpers are framework-free; store and components own runtime/UI state. -->
<!-- Protocol meaning lives in protocol/spec; this file records only web ownership and browser invariants. -->

# apps/web — Solid SPA

The browser client. Vite builds `apps/web/dist`; coord and the worker local door serve that build. The SPA paints cell frames prepared by a worker. It does not run a terminal core, parse PTY bytes, or own coordinator worker state.

Protocol contract index: [`protocol/README.md`](../../protocol/README.md). Normative endpoint and wire behavior: [`protocol/spec/sync.md`](../../protocol/spec/sync.md), [`protocol/spec/terminal-stream.md`](../../protocol/spec/terminal-stream.md), [`protocol/spec/direct-terminal.md`](../../protocol/spec/direct-terminal.md), [`protocol/spec/attachments.md`](../../protocol/spec/attachments.md), [`protocol/spec/auth-and-pairing.md`](../../protocol/spec/auth-and-pairing.md), and [`protocol/spec/agent-metadata.md`](../../protocol/spec/agent-metadata.md).

## Entry point

`apps/web/src/entry.ts` scrubs the URL credential before loading `apps/web/src/main.tsx`. `main.tsx` imports global styles, installs browser diagnostics, applies the saved theme, initializes the local terminal fast path, and mounts `App.tsx`. `App.tsx` installs the access gate, pairing requester/approver providers, Sync bootstrap, routes, and the workbench shell. `routes.ts` is the single URL route table; `MainPane.tsx` is the shared route-array page so terminal renderers survive session/file/search navigation.

## Module map

One row per current owned source or test directory. `Owns` names the directory's responsibility; `Must not own` names the adjacent layer that must remain outside it.

| Directory | Owns | Must not own |
| --- | --- | --- |
| `apps/web/src/` | Root entry, mount, route, access-gate composition, ambient declarations, and root shell files. | Framework-free client logic or feature state owned below `client/`, `store/`, or `components/`. |
| `apps/web/src/browser/` | Browser capability adapters: visibility, clipboard, push, input/gamepad, file download, leak/offline watches, and browser diagnostics. | Application state, Solid components, or protocol parsers. |
| `apps/web/src/client/` | Framework-free client-layer boundary; its child directories own transport, auth, and pure protocol-adapter helpers. | `.tsx` UI, Solid primitives, or mutable application stores. |
| `apps/web/src/client/agents/` | Agent status and notification projection helpers. | Root-store mutation, UI rendering, or public transcript data. |
| `apps/web/src/client/attachments/` | Browser-safe direct attachment selection, grant, packet, peer, and insertion helpers. | UI components or coordinator-owned durable state. |
| `apps/web/src/client/auth/` | Web key persistence, fragment credentials, pairing ceremony records, approver evidence, and tab identity. | RPC dispatch in `client/rpc/` or UI component state. |
| `apps/web/src/client/carriers/` | Loopback discovery, direct terminal peer helpers, packet budgets, and attachment-loopback carrier. | Route election or canonical replica state owned by `store/`. |
| `apps/web/src/client/input/` | Terminal input encoding, history, predictive echo, and input status helpers. | Direct transport admission or component event handling. |
| `apps/web/src/client/rpc/` | Connect-RPC client transport and generated request plumbing. | Sync state, auth ceremony storage, or UI rendering. |
| `apps/web/src/client/search/` | Terminal find intent, handoff, and paging helpers. | Search page composition or global content-search controller state. |
| `apps/web/src/client/sync/` | Sync proto adapters, flow negotiation, and frame dispatch helpers. | Coordinator server implementation or root-store ownership. |
| `apps/web/src/client/terminal-stream/` | Cell geometry, history ranges, backfill state, and stream frame folding. | Browser DOM rendering or direct-route election. |
| `apps/web/src/client/ui-state/` | Portable UI layout apply core and validation helpers. | Browser persistence, navigation, or coordinator UI handlers. |
| `apps/web/src/components/` | Root route page, shared dialogs, context-menu primitives, UI bridge, and component composition. | State mutation or protocol implementation. |
| `apps/web/src/components/agents/` | Agent/task editor components and agent status indicators. | Agent protocol schemas or root-store definitions. |
| `apps/web/src/components/browse/` | Worker browse pages, folder creation, file viewer, and file glyphs. | Filesystem RPC implementation or terminal rendering. |
| `apps/web/src/components/deck/` | Terminal deck, pane tabs, split geometry, arranging, and deck gestures. | Direct carrier election or terminal cell generation. |
| `apps/web/src/components/design/` | Design gallery and visual specimens for design-system verification. | Product navigation or new raw design tokens. |
| `apps/web/src/components/layout/` | Desktop and compact workbench shells, rails, title/status bars, resizers, and mobile top bar. | Route-specific terminal or settings content. |
| `apps/web/src/components/machines/` | Machine identity marks, enrollment readiness dialog, and local access guide. | Coordinator registry or global machine state. |
| `apps/web/src/components/notifications/` | Notification dock, toasts, transfer cards, version/dialog surfaces, and agent notification bridge. | Transfer protocol, terminal state, or independent positioning policy. |
| `apps/web/src/components/pairing/` | Pairing requester/approver providers, onboarding, verification code dialog, and request cards. | Secret hashing, coordinator authorization, or URL credential scrubbing. |
| `apps/web/src/components/palette/` | Command palette, help overlay, controller map, and command-palette presentation. | Command protocol definitions or root-store mutation. |
| `apps/web/src/components/search/` | Global search page and content-result presentation. | Search fanout, worker cursor policy, or agent transcript inspection. |
| `apps/web/src/components/Settings/` | Settings shell, navigation, panes, and mobile list/detail flow. | Settings schemas, token declarations, or data fetching outside the shell. |
| `apps/web/src/components/Settings/md/` | M3 primitives, settings token CSS, icon/control CSS, and shared loading primitive. | App state, data fetching, or new global theme-token declarations. |
| `apps/web/src/components/sidebar/` | Machine, folder, session, agent lists, search, and row context menus. | Per-view stores; selection and filtering derive from URL and `rootStore`. |
| `apps/web/src/components/terminal/` | CellTerminal composition, viewport, renderer attachment, input, lifecycle, selection, and terminal menus. | Direct route election, worker PTY ownership, or protocol constants. |
| `apps/web/src/lib/` | Remaining UI-side helpers: layout/UI commands, search controller, attachments, agent notices, browse/deck helpers, and browser-safe formatting. | Framework-free client modules that belong under `client/`; canonical protocol schemas. |
| `apps/web/src/renderer/` | CellGridRenderer, row rendering, scroll ownership, links, selection, predictive echo, terminal input presentation, and incident evidence. | VT parsing, PTY bytes, protocol generation, or transport state. |
| `apps/web/src/smoke/` | Browser smoke harness, probes, runtime controls, and terminal fixture helpers. | Production application behavior. |
| `apps/web/src/store/` | Single reactive root store, selectors/mutations/projector, Sync leaves, terminal replicas, pane layout, UI, and browser-access state. | UI rendering, transport implementation, or a second root store. |
| `apps/web/src/store/auth/` | Stateful pairing redemption, approval lifecycle, and auth boundary state. | Web-key crypto helpers in `client/auth/` or UI components. |
| `apps/web/src/store/prefs/` | Terminal font, mouse, notification, predictive echo, copy, resize, and keyterm preferences. | Durable application state or transport policy. |
| `apps/web/src/store/transport/` | Direct registry, loopback/local terminal, Sync outbound, terminal input router, route claims, and peer promotion/fallback. | Protocol schemas, worker transport implementation, or canonical replica state. |
| `apps/web/src/styles/` | Global theme variables and workbench/sidebar/voice/gamepad/settings styles imported by the app shell. | Component-local one-off styles or unapproved raw design tokens. |
| `apps/web/public/` | Static fonts, icons, manifest, push worker, What's New data, and pinned wterm WASM. | Generated Vite output or mutable application state. |
| `apps/web/tests/` | Recursive Bun unit and fake-DOM suites for current web modules. | Browser-real Playwright assertions. |
| `apps/web/tests/components/` | Component and browse-focused tests. | Shared fixtures and production components. |
| `apps/web/tests/renderer/` | Renderer, scroll, find, link, input, and terminal presentation tests. | Transport or worker tests. |
| `apps/web/tests/client/` | Client-layer tests grouped by client subdomain. | Root-store integration tests. |
| `apps/web/tests/store/` | Store and auth-state tests. | UI component tests. |
| `apps/web/tests/browser/` | Browser adapter tests. | Browser-real Playwright tests. |
| `apps/web/tests/voice/` | Voice and dictation tests. | Terminal carrier tests. |
| `apps/web/tests/helpers/` | Shared non-suite fixtures and fake DOM/Solid harnesses. | Test registration. |

## Invariants

- `apps/web/src/renderer/cellRenderer.ts` is one class and remains under the file-size baseline; its private per-frame reader and scroll state are one encapsulation. Only `_pinToBottom()`/`_writeScrollTop()` may assign `scrollTop`, with the follow-band and `terminalReaderScroll.ts` exception documented by that module.
- The renderer paints worker-supplied cell rows and scrollback. It never parses VT, re-reflows history, mirrors the worker grid, or receives raw PTY bytes.
- `apps/web/src/store/root.ts` is the only `createStore<RootState>`. Components use selectors and named mutations; `store/projector.ts` uses the shared protocol event fold.
- A terminal direct candidate must fold a complete valid baseline before atomic promotion. Route loss retires the exact token and starts fresh Sync baseline repair; hidden panes may stop receiving cells but their canonical replicas are not deleted.
- One input router owns started input, bounded unsent work, route claims, and exact token settlement. Ambiguous input is not retried.
- Browser-local layout state is validated through `@roost/protocol/layout-document`; runtime pane/split IDs never cross the wire. Acknowledged apply targets one current fingerprint/tab/Sync-v2 socket and is attempted once.
- `client/` is framework-free. `.tsx` and Solid/Kobalte imports stay out of that tree; `components/` is the only UI directory allowed to import other components.
- Only visible panes publish active terminal views. `NotificationDock` is the single bottom overlay column; `DesignGallery` is the design-system reference.

Normative carrier, event, pairing, attachment, and terminal-stream rules belong to the linked `protocol/spec` files. This README records where browser implementation lives, not a second copy of those state machines.

## Test and proof

`bun test apps/web/tests/` runs the recursive Bun suite. `smoke/terminal/terminal-delivery.spec.ts`, `smoke/terminal/terminal-render*.spec.ts`, and `smoke/terminal/terminal-input.spec.ts` exercise the real coord/worker/keeper/PTY/browser tier. Build with `bun run --cwd apps/web build`; use `bun smoke/terminal/live-stack.ts` for the hands-on stack.
