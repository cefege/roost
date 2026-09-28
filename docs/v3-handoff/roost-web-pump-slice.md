# The web track's next slice: the host pump

Written buildless, at `v3-web@43c1a5b7`, while Track U yields the build slot.
Every claim below is grep or file-read. Nothing here is a compiler result.

## Why this slice and not a surface

`roost-web` is 4,757 lines across 30 files and renders nothing, permanently.

- `grep -rn '\.handle(' crates/roost-web/src/` — **0 matches.** The core is
  constructed (`lib.rs:101-107`), provided as context, and read at
  `app.rs:195-197` (`read_access` = `core.borrow().store().browser_access_state`).
  Never driven.
- No `use_future`, no `spawn`, no `set_interval`, no `use_effect` anywhere. (The
  two `spawn` grep hits are the field name `session.spawn_cwd`.)
- `Gate::Checking` is the default, so `GatedApp` renders `CheckingScreen`,
  `AuthorizedShell` never runs, `ServedSurface::Home` never renders.
- `app.rs:126-133` says this itself: "the host pump that turns Sync frames and
  RPC results into client events is not here — so the store does not change and
  the gate holds at `Checking` until that pump lands."

So the order is fixed: **pump → gate opens → Home renders → then a second
surface.** A second surface now is a second unreachable surface.

## ITEM 1 — the re-render signal, and why the render is caused not assumed

Dioxus re-runs a component when a `Signal` it **read during render** changes.
Two facts in this crate fix the design, and they are in tension:

- `lib.rs:10-13`: "The store's `revision` is the only thing a component
  subscribes to. A component … reads that session's `frame_revision` and
  compares it; it never polls."
- `router_state.rs:14-16`: "The signal is the render source of truth, not
  `location`, because **only the signal re-renders**."

`store().revision` is a plain field inside `Rc<RefCell<ClientCore>>`. A Dioxus
render never sees that change. So the two must be bridged, and the pump owns the
bridge.

**Decision: a `Signal<u64>` revision in context, bumped by the pump after every
`handle`, and read during render by the components that display store state.**

- The crate's own idiom is already this shape: `router_state::navigation_handler`
  closes over a `Signal<String>` and calls `path.set(next)` to move the rendered
  path (`router_state.rs:44-53`). Same mechanism, same reason.
- **The failure this design exists to prevent** is the one this track is named
  for: a signal that nobody reads during render. Bumping it forever repaints
  nothing, and the build is green. So the read is part of the slice, not an
  afterthought — the revision must be read during render by whatever displays
  store state, or the pump is inert.
- **Causation, not assumption:** the bump is sequenced *inside* the same call
  that performed `handle`, immediately after it returns, not on a timer and not
  on a separate task. Every handled event therefore produces exactly one render
  pass. A `handle` that changes nothing still bumps — re-rendering an identical
  tree is correct, and skipping the bump would leave a stale render behind. The
  dangerous alternative (poll on an interval) is what `lib.rs:12-13` already
  forbids in the component layer.
- Precedent for reporting a refused browser operation: `router_state.rs:75-77`
  uses `tracing::warn!(target: "router", …)`. The pump's socket failures get the
  same treatment, not a silent drop.

## ITEM 2 — the decode path does not exist, and the layering says where it cannot go

**The decode is not written.** And it is not writable where the code comment
says it belongs:

- `sync/inbound.rs:3-8`: "**The host decodes protobuf; the core decides.**"
  `SyncFrame` there is the already-*decoded* typed vocabulary.
- `ClientEvent::SyncFrameReceived` (`event.rs:54`) carries a decoded frame.
- The step `Vec<u8> -> SyncFrame` therefore belongs to the host = `roost-web`.
- **`roost-web` cannot do it.** `xtask/src/crate_dag.rs` declares `roost-web` may
  depend on exactly `roost-web-terminal`, `roost-client-core`, `roost-protocol`.
  `roost-proto` is **not** on that list, and `roost-web`'s `Cargo.toml` does not
  depend on it. No `roost_proto`/`prost::` decode exists anywhere in the crate.

So `inbound.rs`'s "the host decodes" and `crate_dag`'s "the host may not see
roost-proto" are in direct conflict, and that conflict is the shape of the slice.

**Decision: the decode goes in `roost-client-core`, which already may depend on
`roost-proto` (`crate_dag.rs:114-117`) and already owns the adjacent code —
`handle_sync.rs`, `sync/inbound.rs`, `client/sync/frame.rs`.** This needs **no DAG
change**, and it puts the decode beside the `ClientEvent` variant it produces.

The alternative — adding `roost-proto` to `roost-web` — is a one-line DAG change
and a one-line Cargo.toml change, but it moves wire types into the component
crate, which is the direction the enforcer exists to prevent. Recorded as
rejected, with the reason, rather than silently skipped.

**So the slice is two crates, not one:**

1. `roost-client-core` — a decode entry point, `bytes -> ClientEvent` (or
   `-> SyncFrame` + `SyncFrameMeta`), with tests. `SyncFrameMeta` matters:
   `client/sync/frame.rs:1-19` records that a frame arriving without it must be
   **refused at the door**, not queued unplaceable, or the recovery cursor stops.
2. `roost-web` — the pump: own the `WebSocketSyncSocket`, drain
   `SyncSocketMessage { Open, Binary, Closed }`, call decode, call
   `core.handle(event)`, execute the returned `Vec<Effect>`, bump the revision
   signal.

The `Vec<Effect>` half is why this is a pump and not a decode: the core does no
I/O, so it asks the host to do it. Both directions must be driven.

## What is already unwired, and is part of this

Of the 8 `platform/` modules, the five that perform I/O have **no caller outside
`platform/`**: `carrier` (0), `fragment_credential` (0), `peer` (0), `rpc` (0),
`sync_socket` (0). `clock` and `storage` *are* constructed by `build_core`, and
`location` has one caller — those are not dead.

`platform/sync_socket.rs` is 340 lines of transport the pump will own.

## What a build would have to answer, and cannot be answered here

- Does `roost-web` compile at all? Unknown. 39 tests across 3 files have never
  been run by any gate: the U-2 gate named only `roost-client-core` and
  `roost-web-terminal`, while `ci.yml:44` names all four. `ci.yml:39-42` already
  says why that matters: "A gate that is green because it is absent is not a gate."
- Does the `Binary` path decode cleanly, or is part of it another never-compiled
  region like `roost-web-terminal`'s 414 wasm-gated lines?
- Are the 39 `roost-web` tests green?

I have no compiler, so these are open. A red result on
`cargo test -p roost-web --no-fail-fast` is the *good* outcome: it is a real
answer about the track rather than a green over a hole.
