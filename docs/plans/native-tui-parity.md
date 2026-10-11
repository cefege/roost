# Terminal client, models hub and OMP parity

Branch `v3-native-parity`, cut from `v3` at `c9986b1b`. Every slice below lands
here first; `v3` is unchanged until the branch merges.

## Decisions

| Area | Decision | Why |
|---|---|---|
| Terminal client | `roost-client-native` (the tokio host for `roost-client-core`: clock, file store, Sync socket, Connect transport, device key) and `roost-tui` (ratatui + crossterm), shipped as `roost tui`. | Cell shipping means the coordinator already sends folded cell grids, so the TUI paints `CellGridFrame` spans straight into a ratatui buffer and needs no terminal emulator. The key encoders move from `roost-web-terminal` into `roost-client-core` so both clients share them. |
| Models hub | Built from OMP's `/models` hub and picker: Recent / Roles / All / per-provider sidebar, fuzzy search, metadata columns, detail pane, role → scope → thinking assignment, per-model thinking levels, presets, cycle order, enabled models and disabled providers, a locked provider opens sign-in. | The `ModelEntry` wire is widened first. Ranking and grouping live in `roost-protocol` so the web and TUI share them. |
| OMP parity | Port OMP harness behavior where it fits an agent loop on the coordinator and tools on the folder's worker. What is not ported is listed with its reason in `docs/OMP-PARITY.md`. | The split stays: anything that touches the project's files, processes or binaries is a worker tool; conversation state, model calls and persistence stay on the coordinator. |

## Slices

| Slice | Owns |
|---|---|
| Models hub | the `ModelEntry`/catalog wire, `rpc_accounts`, `/models` `/switch` `/modelpreset`, the web hub and picker |
| Terminal client | `crates/roost-client-native`, `crates/roost-tui`, `roost tui`, the encoder move |
| Project profile | context-file discovery parity, rules, skills, custom agents and task parameters |
| Worker tools | background bash + `wait` + `/jobs`, rich `read`, `ast_grep`/`ast_edit`, `github` |
| Images | typed tool-result content parts, image reads, image attachments in chat |
| MCP | stdio servers on the worker, HTTP servers from the coordinator, `/mcp` |
| Harness commands | compaction modes, `/context` `/tools` `/session` `/todo`, follow-up queue, service tier, `think`, goal mode |
| Harness intelligence | TTSR stream rules, memory, secret obfuscation, shell hooks, checkpoints |

Native desktop apps (macOS, Windows, Linux) are out of scope for this branch by
operator decision. The investigated route, kept for when they are picked up: a
wry + tao shell that loads the coordinator's own `https://` origin, because the
web crates are wasm-only and a custom-protocol origin is refused by CORS and
Sync admission. The unfinished crate is parked on branch `np/desktop-shell`
(commit `e44ee483`, never compiled: this host has no WebKitGTK headers).

Each slice is built in its own worktree and merged here. The gates in CLAUDE.md
apply to each merge.
