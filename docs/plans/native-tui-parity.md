# Native apps, terminal client, models hub and OMP parity

Branch `v3-native-parity`, cut from `v3` at `c9986b1b`. Every slice below lands
here first; `v3` is unchanged until the branch merges.

## Decisions

| Area | Decision | Why |
|---|---|---|
| Desktop app | `roost-desktop`: a wry + tao shell (the webview stack Dioxus desktop is built on) that opens the coordinator's own `https://` origin, or the worker door on `127.0.0.1:4114` when one answers. The UI inside is the existing Dioxus web app. | `roost-web` and `roost-web-terminal` are wasm-only: 177+ files call `web-sys`, and device keys are WebCrypto keys in IndexedDB. A `dioxus-desktop` port would fork the platform layer and the terminal renderer (CLAUDE.md rule 12). A custom-protocol origin is refused by CORS and Sync admission, and an embedded bundle drifts from the coordinator version. |
| GPU | The webview compositor: WebView2 (Chromium) on Windows, WKWebView (Core Animation) on macOS, WebKitGTK 4.1 on Linux. Under Hyprland with the NVIDIA driver, the shell re-execs itself with `WEBKIT_DISABLE_DMABUF_RENDERER=1`. | The terminal is a DOM renderer that is already composited (`translate3d`, `will-change`, `contain`). Blitz / dioxus-native is pre-1.0 and not in the tree. |
| Terminal client | `roost-client-native` (the tokio host for `roost-client-core`: clock, file store, Sync socket, Connect transport, device key) and `roost-tui` (ratatui + crossterm), shipped as `roost tui`. | Cell shipping means the coordinator already sends folded cell grids, so the TUI paints `CellGridFrame` spans straight into a ratatui buffer and needs no terminal emulator. The key encoders move from `roost-web-terminal` into `roost-client-core` so both clients share them. |
| Models hub | Built from OMP's `/models` hub and picker: Recent / Roles / All / per-provider sidebar, fuzzy search, metadata columns, detail pane, role → scope → thinking assignment, per-model thinking levels, presets, cycle order, enabled models and disabled providers, a locked provider opens sign-in. | The `ModelEntry` wire is widened first. Ranking and grouping live in `roost-protocol` so the web, desktop and TUI share them. |
| OMP parity | Port OMP harness behavior where it fits an agent loop on the coordinator and tools on the folder's worker. What is not ported is listed with its reason in `docs/OMP-PARITY.md`. | The split stays: anything that touches the project's files, processes or binaries is a worker tool; conversation state, model calls and persistence stay on the coordinator. |

## Slices

| Slice | Owns |
|---|---|
| Models hub | the `ModelEntry`/catalog wire, `rpc_accounts`, `/models` `/switch` `/modelpreset`, the web hub and picker |
| Desktop shell | `crates/roost-desktop`, the desktop jobs in `release.yml`/`ci.yml`, install docs |
| Terminal client | `crates/roost-client-native`, `crates/roost-tui`, `roost tui`, the encoder move |
| Project profile | context-file discovery parity, rules, skills, custom agents and task parameters |
| Worker tools | background bash + `wait` + `/jobs`, rich `read`, `ast_grep`/`ast_edit`, `github` |
| Images | typed tool-result content parts, image reads, image attachments in chat |
| MCP | stdio servers on the worker, HTTP servers from the coordinator, `/mcp` |
| Harness commands | compaction modes, `/context` `/tools` `/session` `/todo`, follow-up queue, service tier, `think`, goal mode |
| Harness intelligence | TTSR stream rules, memory, secret obfuscation, shell hooks, checkpoints |

Each slice is built in its own worktree and merged here. The gates in CLAUDE.md
apply to each merge.
