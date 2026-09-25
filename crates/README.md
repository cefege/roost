# `crates/` — the v3 crate map

Thirteen crates plus a gate runner. One Git repository, one GitHub repo, one
wire contract; the v2 TypeScript tree in `apps/` and `packages/` is still here
while the port runs and is deleted in Phase 7.

Every crate is named for the concept it owns. `Worker` is a machine in the
registry, `Session` is the user-facing row, `Channel` is a PTY connection,
`Tab` is the database row — one concept per type, converted at boundaries.

| Crate | Owns | Binary |
|---|---|---|
| `roost-proto` | Generated protobuf messages and Connect service stubs, built from `protocol/proto/roost/v1` by `connectrpc-build`. | — |
| `roost-observability` | `tracing` JSON-lines init and the log line shape `roost status` / `roost doctor` parse. | — |
| `roost-protocol` | Pure I/O-free wire and terminal logic: event variants and the canonical fold, cell and grid models, viewport geometry, peer packet framing, the keeper-update contract, layout documents. Builds for `wasm32`. | — |
| `roost-platform` | Platform path and shell conventions. No I/O, no async, no logging. | — |
| `roost-host` | `ROOST_*` config, service names, durable atomic writes, service health probes. | — |
| `roost-term` | The `TerminalCore` trait, its Alacritty implementation, and the grid→`CellGridFrame` emitter. | — |
| `roost-keeper` | The keeper daemon: PTY ownership, per-channel byte rings, the framed keeper socket protocol. | `roost-keeper` |
| `roost-worker` | Sessions, keeper client, durable outbox, coordinator link, local door, WebRTC peer, agent tracking. | — |
| `roost-coord` | SQLite state, auth, Connect RPC handlers, Sync and worker WebSocket links, terminal hubs, web push. | — |
| `roost-client-core` | The UI-free client: Connect client, Sync state machine, store fold, terminal-stream replica and route election, input lanes, encoders, find paging. | — |
| `roost-web-terminal` | The imperative `web-sys` terminal renderer and its input, IME, mouse, selection and link controllers. | — |
| `roost-web` | The Dioxus 0.7 web application: routes, components, static assets. | — |
| `roost-cli` | The `roost` binary: `coord`, `worker`, `deploy`, `status`, `doctor`, and the rest. | `roost` |

## Dependency DAG

Dependencies point one way. The list below is not documentation — it is the
allowlist in `xtask/src/crate_dag.rs`, and a new edge fails
`cargo xtask lint` until it is registered there deliberately.

```text
roost-proto          → ∅
roost-observability  → ∅
roost-platform       → ∅
roost-protocol       → proto, observability
roost-host           → protocol, platform, observability
roost-term           → protocol, observability
roost-keeper         → protocol, host, observability
roost-worker         → term, keeper, host, protocol, platform, observability
roost-coord          → host, protocol, platform, observability
roost-client-core    → protocol, proto, observability
roost-web-terminal   → client-core, protocol
roost-web            → web-terminal, client-core, protocol
roost-cli            → coord, worker, keeper, host, protocol, platform, observability
```

`roost-client-core` is the seam every future front end links: it exposes no
`web-sys` and no `tokio` I/O types in its public API, and reaches the host
through traits, so the same state machine drives a wasm browser, a tokio TUI,
and a mobile host. A future native app depends only on `roost-client-core` and
`roost-protocol`.

## Rules that apply to every crate

- `#![forbid(unsafe_code)]` in the crate root. The two exceptions are
  `roost-keeper` (it owns raw file descriptors and the controlling-TTY
  handshake) and anything under `third_party/`.
- A `//!` file header of 3–6 lines: what this file owns, what calls it, what
  it depends on.
- ≤400 lines per file, enforced by `cargo xtask lint` against
  `xtask/file-size-baseline.json`.
- `thiserror` in libraries, `anyhow` only in a binary's `main`.
- Tests mirror source layout: `#[cfg(test)]` for unit tests,
  `crates/<x>/tests/<mirror>.rs` for behaviour tests.
