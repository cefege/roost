# Wave B contract (Stage 5 U-1 SMOKE/TERM + U-2 SHELL/SIDEBAR/DECK)

Read `/tmp/web-slice-context.md` first; every rule there binds you. This file adds the wave-B
interfaces. The lead is `agent://WebLeadU2`; siblings are `agent://WebLeadU2.<Name>`.

## The gate this wave exists for
`smoke/terminal/terminal-delivery.spec.ts` "browser smoke flow creates and cleans its resources",
run against the Rust bundle `crates/roost-web/dist-smoke` (built with `--features smoke`) over the
TS coord+worker. Its fixture (`smoke/terminal/fixtures.ts:134-177`) waits for, in order:
`location.hash === ""` after `/#pair=<token>`; `window.__smoke.state().workers[fp]` truthy;
`.workbench-shell[data-compact="true"|"false"]`; exactly one `[data-testid="folder-list"]`
(visible when not compact); zero `[data-testid="error-boundary"]`. Then the spec calls
`window.__smoke.runFlow(...)` (v2 `apps/web/src/smoke/smokeHarness.ts`).

## Crates and state
- `roost-client-core` owns the store (`ClientCore`, `Store`, selectors, RPC call builders under
  `client/rpc/calls/`) — framework-free, native tests. `roost-web` owns Dioxus components and the
  pump (`crates/roost-web/src/pump.rs`: `use_pump()` returns the pump with its revision read so
  the caller re-renders; `pump.core()` is `Rc<RefCell<ClientCore>>`; `pump.dispatch(ClientEvent)`
  feeds the core and executes its effects). `roost-web-terminal` owns the imperative renderer
  (`CellGridRenderer`, input controller, mouse forwarding, echo host, find, links, backfill).
- New store state goes in NEW client-core files you own (`store/<concept>.rs`); the only edits to
  shared files (`store.rs`, `lib.rs`, `mod.rs`, `components/mod.rs`, `Cargo.toml` feature lists)
  are one-line registrations, re-read before editing, listed in your report.
- A new user action = a `ClientEvent` variant (or an existing RPC call builder) handled in
  client-core with a native test; the component only dispatches it.

## Component interfaces (v2 name, snake_case props; module path mirrors v2's dir)
Keep v2's component names, class names and `data-testid`s exactly.
- TERM `crate::components::terminal::terminal_card::TerminalCard` and
  `crate::components::terminal::cell_terminal::CellTerminal` (props = v2 `TerminalCard.tsx` /
  `CellTerminal.tsx` props, snake_cased); `terminal_transport_indicator::TerminalTransportIndicator`.
  TERM also owns the session-keyed pane registry SMOKE reads (renderer handle per session:
  viewport text, painted-marker probe, render probe, cell frame counts).
- DECK `crate::components::deck::terminal_deck::TerminalDeck` (props = v2 `TerminalDeck.tsx`).
  Renders TERM's `TerminalCard`/`CellTerminal`. The compose button slot belongs to the later
  COMPOSER slice: omit it and say so in your report.
- SIDEBAR `crate::components::sidebar::sidebar_root::SidebarRoot` (props = v2 `SidebarRoot.tsx`),
  renders `data-testid="folder-list"`. SIDEBAR also ports the three leaf components it imports:
  `agents/AgentStatusIndicator.tsx`, `browse/FolderGlyph.tsx`, `machines/MachineIdentityMark.tsx`
  into `components/{agents,browse,machines}/<snake>.rs` (leaf files only; the rest of those dirs
  belong to later slices).
- SHELL owns `app.rs`, `routes.rs`, `router_state.rs`, `components/layout*`, `components/{home,
  not_served,access_gate}.rs`, and new `components/{main_pane,rename_dialog,app_error_boundary,
  ui_bridge,context_menu}` + `motion/`. `AppShell` renders `.workbench-shell[data-compact]`,
  `SidebarRoot` and `MainPane`; `MainPane` renders `TerminalDeck`; `AppErrorBoundary` renders
  `data-testid="error-boundary"` only on a caught error.
- SMOKE owns `crates/roost-web/src/smoke/**` (behind `#[cfg(feature = "smoke")]`, installed only
  when `localStorage.roostSmoke === "1"`) and new client-core files for v2
  `store/sync-smoke.ts` and `client/carriers/sync-outbound-smoke.ts`.

## Integration order (avoid breaking the shared crate build)
The crate is shared: never leave a `pub mod` registration pointing at a file that does not compile.
A consumer wires a producer's component only after `grep -n 'pub fn <Name>'` finds it and the crate
checks; until then message the producer (`write agent://WebLeadU2.<Name>`) and keep working.
Producers: tell your consumers (message) the moment your component compiles. If `c check` fails
only in another slice's files, wait (block on the lock) and retry; do not edit their files.
