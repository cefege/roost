<!-- AUDIENCE: human -->
# Architecture

A tour of how Roost fits together. For build/run, see
[`GETTING_STARTED.md`](GETTING_STARTED.md); for terms,
[`GLOSSARY.md`](GLOSSARY.md). The exhaustive in-repo reference (for LLM
collaborators) is [`CLAUDE.md`](CLAUDE.md).

Roost has three product processes, one operator CLI, and one shared protocol
package. The browser SPA is served by the coordinator; coordinator and worker
processes run on Bun.
Path references in this document are repo-root-relative.

```
  Browser (Solid SPA on any device that can reach the operator's front door)
       │
       │  unary Connect-RPC + protobuf Sync WebSocket (HTTPS)
       ▼
  Front door (Caddy, nginx, a tunnel, tailscale serve — not Roost's code)
       │  plaintext HTTP to 127.0.0.1:4103
       ▼
  Coordinator (Bun)
       │
       │  WebSocket + protobuf (worker link)
       │
       ├──────────────────────┬──────────────────────┐
       ▼                      ▼                      ▼
  Worker (Bun)           Worker (Bun)           Worker (Bun)
  macOS/Linux            macOS/Linux            macOS/Linux
       │                      │                      │
       │ Unix socket          │ Unix socket          │ Unix socket
       ▼                      ▼                      ▼
  Keeper daemon          Keeper daemon          Keeper daemon
       │                      │                      │
       └─ PTY per session     └─ PTY per session     └─ PTY per session
```

This diagram shows the always-present control plane. A selected terminal route
may instead carry cell frames and input directly from the browser to its worker:
same-worker loopback first, then an authenticated WebRTC UDP peer; Sync remains
connected for metadata, authorization/control, and fallback.

The coordinator owns one listener: a plaintext loopback bind. TLS, DNS, and
public reachability belong to whatever the operator puts in front of it, and
`ROOST_WEB_PUBLIC_URL` is how the coordinator learns the resulting origin.

## Product processes and shared protocol

### `apps/web` — the browser client

The SolidJS SPA owns operator interaction: workspaces, sessions, terminal
painting, settings, and dashboard selection. It keeps Sync for authenticated
metadata, control, and fallback, then paints typed cell snapshots/deltas from
the elected terminal carrier; raw PTY bytes never reach the browser.

### `apps/coord` — the control plane

The coordinator owns the durable event log and projections, authentication,
dashboard authorization, workspace/task metadata, worker presence, Sync
fan-out and its terminal replicas, plus direct-terminal grant admission,
bounded peer signaling, and worker-generation fences. It routes terminal
commands but never owns the PTY or terminal parser.

### `apps/worker` — one per host

Each v0.5.0 worker runs on macOS or Linux, owns the keeper link and
`@wterm/core` terminal state, persists crash-sensitive lifecycle delivery, and
maintains the outbound coordinator link. Its loopback UI door serves
same-machine direct terminals; only after coordinator admission may it create a
bounded authenticated WebRTC UDP peer. Windows host release support is paused;
Windows remains usable as a browser client.

### `apps/roost-cli` — install, health, and rollout

The `roost` binary installs the local coordinator/worker pair, reports
`status` and `doctor`, and performs journaled fleet rollout.

### `protocol/` + `packages/` — the portable contract

Language-neutral proto sources live in `protocol/proto`; wire schemas,
generated bindings, event folding, terminal cells, direct-peer limits, platform
rules, observability, and host configuration live in explicitly layered
packages under `packages/`. Consumers import the package and subpath that own
the concern rather than a barrel.

## Event sourcing

Durable session state is an ordered, ACK-paced event log reconciled by a sequenced worker snapshot before live traffic.
The normative worker/coordinator/browser fold and private-reference boundary are in [`protocol/spec/session-events.md`](protocol/spec/session-events.md).
## The terminal data plane

**Carrier selection.** Sync remains the control/fallback plane; direct terminal uses a current grant and validated full before promotion.
The normative carrier, security, STUN, and outage rules are in [`protocol/spec/direct-terminal.md`](protocol/spec/direct-terminal.md).

**Direct security boundary.** Grants and signaling bind the exact device, tab, worker connection, process epoch, and live worker-side recheck.
The normative boundary is in [`protocol/spec/direct-terminal.md`](protocol/spec/direct-terminal.md).

**STUN and ICE are opportunistic discovery.** STUN discovers addresses only; Roost supplies no TURN relay or universal direct-path guarantee.
The exact bounded configuration is in [`protocol/spec/direct-terminal.md`](protocol/spec/direct-terminal.md).

**Three explicit replicas.** The worker core is authoritative; coordinator and browser each retain one session replica, while direct candidates fold separately.
The replica boundary is normative in [`protocol/spec/terminal-stream.md`](protocol/spec/terminal-stream.md).

**One membership and geometry authority per session.** Worker-owned or coordinator-owned views compute the per-axis smallest common denominator with bounded leases and park grace.
The membership and geometry contract is normative in [`protocol/spec/terminal-stream.md`](protocol/spec/terminal-stream.md).

**Full before delta.** Each stream generation requires one complete full; only exact stream/epoch/geometry/sequence successors extend a replica.
Chunk assembly and stream limits are normative in [`protocol/spec/terminal-stream.md`](protocol/spec/terminal-stream.md).

**Route liveness and direct outage.** Views renew and direct peers probe on bounded schedules; exact route loss starts fresh-baseline fallback without input replay.
See [`protocol/spec/direct-terminal.md`](protocol/spec/direct-terminal.md).

**Bounded resumable delivery.** Oversized fulls use contiguous whole-row chunks with one snapshot identity and independent per-carrier queues.
See [`protocol/spec/terminal-stream.md`](protocol/spec/terminal-stream.md).

**Resize at the keeper's ordered boundary.** The acknowledged keeper resize is the parse boundary; the existing core resizes synchronously and forces a new full.
See [`protocol/spec/terminal-stream.md`](protocol/spec/terminal-stream.md).

**Proven outcomes and independent input.** Worker results distinguish accepted, rejected, and ambiguous writes; only proven pre-write rejection is retry-safe.
Input-route handoff is normative in [`protocol/spec/direct-terminal.md`](protocol/spec/direct-terminal.md).

**Canonical model vs painted DOM.** These remain different clocks and
`apps/web/src/renderer/terminalDiagSnapshot.ts` reports both: view ID/revision/lease
state, coordinator stream ID, browser replica epoch/sequence, and renderer
reconciled epoch/sequence. `apps/web/src/store/terminal-stream-diagnostics.ts`
adds the elected/candidate transport, worker epoch, opaque peer ID, phase,
content-free probe age/RTT, buffered bytes, and fallback reason. A renderer
frozen for a reader may intentionally trail the replica. A same-width,
same-epoch full repair is a new tail checkpoint: already-painted immutable
history rows and their DOM nodes survive while the live viewport is replaced.
Width/epoch changes retain the global reader anchor and refetch it when
necessary.

**Live vs reading.** `CellGridRenderer` carries an explicit `ReaderIntent` plus
holds for selection and an armed link. Passive output and composer drafting do
not cancel a reader. One admitted local keystroke calls
`prepareLiveInteraction()`, adopts pending state and returns to the live tail as
one transition. Leaving the surface ends the reading interval, but it does not
delete the session replica.

**The application decides mouse and focus forwarding.** Cell frames carry the
core's `mouse_tracking`, `mouse_sgr`, `focus_events`, `cursor_keys_app` and
`bracketed_paste` modes. The browser forwards only what the application
requested, using SGR-1006 or bounded legacy X10 as appropriate. Alternate
screen occupancy alone never captures mouse input.

**Publication remains durable and ordered.** Session events commit before channel publication, and the announced-channel barrier preserves first-frame order.
Sync/direct repair and ACK flow control are normative in [`protocol/spec/terminal-stream.md`](protocol/spec/terminal-stream.md) and [`protocol/spec/sync.md`](protocol/spec/sync.md).

Every session remains a shell PTY; agent CLIs such as `omp`, Claude Code, or Codex run inside it manually or through terminal launcher configuration.
Roost never spawns, supervises, or owns an agent process, conversation, transcript, tool call, or approval model.
It may expose volatile worker-observed state, accept occupant-fenced text for the same ordinary PTY, retain an integration-supplied opaque conversation reference as private recovery metadata, and use that reference for one narrowly fenced worker-owned restore input after involuntary PTY loss. None of these surfaces creates a structured agent session.

## Agent conversation references (private recovery metadata)

Official OMP references are validated, durable, worker-private equality metadata used only for one fenced recovery input after keeper adoption fails.
The schema, sequence fold, restore ordering, and feature gate are normative in [`protocol/spec/agent-metadata.md`](protocol/spec/agent-metadata.md).

## Agent status (volatile, metadata only)

Worker-observed `idle|working|blocked` status is PID-free volatile metadata with occupant/revision fencing, bounded waits, and one optional PTY input.
The wire contract and prompt boundary are normative in [`protocol/spec/agent-metadata.md`](protocol/spec/agent-metadata.md).

## Terminal fidelity (the hard part)

Streaming raw bytes to a browser terminal looks simple and corrupts in
practice. The browser re-parses the byte stream at whatever width its own
window happens to be, and **re-parse at a new width is the corruption**: a
terminal core's row resize is asymmetric and lossy — shrinking pushes rows into
scrollback, growing fills with blanks, and neither reverses. No terminal
library reflows a TUI grid to a new width; they all freeze instead. Reconnects
on top of that duplicate or drop output. The only structural fix is to stop
reflowing on the client, so Roost uses the model server-side terminal
multiplexers use:

- The **worker** holds the one authoritative grid per session and rebuilds it at
  a single agreed width on resize.
- The **browser** paints that grid as-is. It parses no VT and never re-reflows;
  surplus pane space is **letterboxed** — rows stay `cols` characters wide and
  the container centres them instead of stretching
  (`apps/web/src/renderer/cellRenderer.ts`). The accepted tradeoff: plain shell
  history no longer rewraps to a narrower device, it scrolls sideways.
- The agreed width is the **SCD** (smallest common denominator) across the
  views that are actually looking, so no present viewer is clipped. One
  `TerminalViewRegistry` owner—worker `TerminalViewOwner` when available,
  otherwise coordinator `TerminalViewHub`—owns membership, and
  `minimumTerminalGeometry` (`@roost/protocol/viewport`) is the one per-axis
  minimum. Leases absorb reconnect wobble while the park grace bounds how long
  a dead viewer's dimensions keep binding, and letterboxing absorbs pixel
  differences without competing resize owners.
- **Alt-screen owns the viewport and carries no scrollback**, so in that mode
  there is nothing to corrupt. The frame states it (`CellGridFrame.altScreen`);
  the renderer hides the history sheet and locks scrolling while it is set, and
  restores both on leaving.
- The cell payload has **one source of truth**, `packages/protocol/src/cell/`:
  `CellSpan` (a run of cells sharing one style, whose style fields mirror the
  core's own `CellData`), `CellRow` (index plus right-trimmed spans), and
  `CellGridFrame` (cols, rows, cursor, alt-screen, viewport rows, scrollback
  append, totals, stream ID, base sequence, sequence and epoch). The protos
  mirror that shape through Sync or a selected direct carrier; neither path
  ships raw PTY bytes to the browser.
- Delivery is **stream-addressed and resumable by exact sequence**. Every
  authoritative full is viewport-only (`baseSeq === 0`,
  `sbBase === scrollbackTotal`), including compatible same-grid renewals.
  Deltas are accepted only when stream ID, epoch, dimensions and `baseSeq`
  match the installed replica and `seq` is its exact successor. Any mismatch
  invalidates the recipient cursor and requests one full snapshot.
  `TerminalScreenHub` keeps the coordinator's Sync replica, while each
  browser's `apps/web/src/store/terminal-stream.ts` replica survives renderer
  detach, direct promotion, and Sync reconnect. Per-byte sequence numbers
  remain one layer lower in the keeper's per-channel ring so a restarted worker
  can re-adopt a live PTY.
- Retained history is **demand-paged**: it is fetched only on explicit scroll
  or find, through the elected carrier (`SessionsGetScrollbackCells` on Sync).
  A cold attach lands at the bottom having fetched none of it, and every
  retained row remains reachable through an epoch-addressed page. An explicit
  fetch crossing the seam runs under the current epoch. A resize while parked
  off-bottom keeps the reader's position and first visible row, but
  de-materialises the rows behind the seam: the retired epoch's held window is
  dropped and a spacer preserves `scrollHeight`. That is reversible on the next
  demand fetch, which is why a render-stress run on the main screen has to let
  the pane settle before it starts — `runRenderStress` captures one marker
  baseline up front and flags any later change of range.
- The **core** is `@wterm/core` 0.5.0, loaded through
  `packages/wterm/src/wterm-core-factory.ts` from a locally patched WASM build
  committed at `packages/wterm/wasm/wterm-roost.wasm`. Its sha256 sits beside it
  in `packages/wterm/wasm/wterm-roost.wasm.sha256`, and `scripts/rebuild-wterm-wasm.sh` reproduces
  the build. Loading is fail-fast: `verifyRoostWasm` rehashes the bytes against
  that digest and checks every 0.5.0 bridge export by name, throwing instead of
  returning a degraded core.
- Column occupancy is **explicit on the wire**: `PbCellSpan.columns` states how
  many terminal columns a span owns, so a double-width glyph is one atomic
  two-column span and no phantom continuation cell is ever emitted.
- The scrollback origin is **authoritative, never inferred**:
  `cell/emitter.ts::scrollbackOrigin` reads the core's
  `getScrollbackDiscardedCount()` and throws if a core cannot supply it, so
  absolute history indices can never re-alias.

This is the part of the codebase with the most scar tissue; the recurring
failure modes and their fixes are catalogued in `CLAUDE.md`.

## Tenant isolation

Coordinator startup creates one local tenant and automatically selects its sole
dashboard: `apps/coord/src/auth/self-hosted-tenant.ts` runs unconditionally at boot
and is the only tenancy invariant. Every resource query, Sync subscription,
worker principal, and terminal route carries that persisted dashboard
boundary.

Authorization never trusts a dashboard id supplied by the browser: it resolves
the persisted account-device membership on the server. Browser dashboard
changes take effect only after server confirmation, clear old scoped state
first, and fence stale async work by Sync generation.

## Portable browser-local pane layouts
Active pane trees and runtime leaf/split UUIDs persist only in each browser profile under `roost.paneLayout.v1`. `UiReportState` exposes an off-terminal route or a route resolved to an open coordinator-admitted session; unresolved and optimistic `/s/:id` route fields are blank until hydration/admission schedules another report. When an open route session identifies a folder, the report also carries its browser-owned folder key plus a typed `LayoutDocumentV1` containing only admitted members, never runtime IDs or a second JSON layout shape.
Copy, download, and reporting serialize the same strict V1 document with deterministic preorder leaf/slot keys and inclusive `0.1..0.9` ratios. One bounded parser rejects excessive UTF-8 identifiers, recursion, nodes, slots, or bindings before recursive conversion. Local import/apply and remote apply additionally validate current live-folder membership; remote apply rejects any pending/tombstoned optimistic member. Successful application materializes fresh runtime IDs, commits once through `applyLayoutDocument`, and attempts post-commit navigation to the focused selection. Open tabs do not consume storage events or live-fold one another's layouts.
`UiApplyLayout` is the sole acknowledged exception to browser-local control: its dashboard-authorized caller sends a nonempty browser fingerprint/tab tuple, and the CLI derives exactly one such tuple from `UiListStates` or rejects an absent/ambiguous tab before apply. Sync rejects tab IDs over 256 UTF-8 bytes before socket state, and the coordinator bounds live targets to 32 distinct tuples per fingerprint and 256 per dashboard with one generic capacity rejection. It reserves only the selected tuple's current authenticated read/write Sync-v2 socket plus a fresh correlation ID before publication, so a stale fingerprint cannot redirect to another browser that later reuses the tab ID. The page rechecks the exact tab/socket/correlation and its URL-active live folder, then acknowledges `applied` after the commit and navigation attempt; `applied` proves the commit, not successful navigation completion, while invalid current membership returns `rejected`.
Wrong, stale, duplicate, and late results are ignored; cancellation removes the pending request, and socket close/replacement or timeout returns `target_gone` without retry. `target_gone` means the acknowledgement is unavailable, not that execution did not occur. UI commands are never seeded, and read-only worker Sync subscribers receive neither UI state nor command frames. The eight `UiDispatch` commands remain fire-and-forget: `delivered` is exactly the dashboard Sync-subscriber count at publication, never an execution or acknowledgement count.

## Resilience model

- **Browser drops:** Sync resumes from the last persisted folded event id, then
  visible terminal liveness challenges repair any stalled pane. Browser page
  lifecycle closes WebRTC peers; a later visible demand stages a new candidate.
- **Direct carrier drops:** the browser retires only the exact loopback or peer
  route, fences its input generation, and returns to Sync through a fresh full
  baseline when Sync is ready. It never replays an input whose prior write was
  accepted or ambiguous.
- **Worker drops:** keeper processes preserve PTYs. Direct routes to that worker
  close; worker reconnect performs durable lifecycle replay, authoritative
  snapshot reconciliation, then live traffic.
- **Worker deletion:** the coordinator sends retirement before deleting the
  credential, and either the confirmed delete response or its presence delta
  independently clears the browser grant, in-flight mint, direct candidates,
  elected routes, and peer retries. A lost retirement frame cannot preserve
  browser-side direct authority.
- **Coordinator drops:** workers retain PTYs and their durable lifecycle
  outboxes; browsers redial while visible. Existing direct routes may remain
  usable only while their installed grants and route liveness remain valid; no
  new direct grant, renewal, or negotiation can complete. The coordinator
  reopens the transactionally persisted event log/projection, and reconnecting
  worker snapshots repair drift.
- **Worker process crashes:** keepers survive independently. The restarted
  worker adopts them and emits `respawned` only when a terminal was actually
  replaced; its new worker epoch invalidates old direct routes.

The design goal is not “nothing ever disconnects.” It is “a disconnect cannot
silently lose a PTY lifecycle edge, and every visible terminal either proves
progress or triggers bounded recovery.”

## Key entry points

- **Web:** `entry.ts` scrubs URL-carried credentials before loading
  `main.tsx`; `routes.ts` owns route guards; `store/sync.ts` owns Sync; and
  `store/auth-boundary.ts` owns credential-boundary teardown.
  `store/terminal-stream-transport.ts` owns the document-scoped direct registry
  and route election, `store/terminal-stream-promotion.ts` stages direct
  baselines, `ws/terminal-peer.ts` owns WebRTC attempts/liveness, and
  `ws/terminal-input-router.ts` owns carrier-neutral input handoff.
  `CellTerminal.tsx` composes the eight `cell-terminal-*` behavior leaves, and
  `lib/cellRenderer.ts` paints the canvas grid.
- **Coordinator:** `main.ts` owns process lifecycle; `connect/router.ts`
  composes the single Connect-RPC service from 18 handler factories;
  `connect/ws-auth-deadline.ts` owns both authenticated WebSocket deadlines.
  `event-{transaction,projection,query}.ts`, `connect/sync-feed.ts`,
  `connect/terminal-screen-hub.ts`, and `connect/auth-principal.ts` own
  persistence, delivery, Sync terminal replicas, and persisted-principal
  resolution. `connect/terminal-grant-owner.ts` owns direct grants and
  invalidation; `connect/terminal-peer-negotiations.ts` owns bounded signaling
  and exact-worker fences.
- **Worker:** `main.ts` is the entry point; `session-manager.ts` owns
  keeper-backed sessions; `transport/session-event-store.ts` owns the bounded
  SQLite lifecycle outbox; `transport/coord-link-unacked.ts` owns the
  replay/snapshot/live barrier; `transport/coord-link.ts`,
  `keeper/multiplexed-client.ts`, and `fsm.ts` own the remote link, local
  keeper transport, and connection state. `boot-local-terminal.ts` composes
  direct ownership; `local-terminal-socket.ts` admits both carriers;
  `terminal-peer-owner.ts` owns native peers; and
  `terminal-input-route-owner.ts` fences direct writers.
- **CLI:** `main.ts` dispatches commands; `quickstart-endpoint.ts` validates the
  one declared front-door origin; `push.ts` and `push-fleet-rollout.ts` own the
  journaled rollout of every reachable worker, and `push-fleet-plan.ts` owns the
  participant/deferred partition that lets an offline machine catch up later
  instead of blocking the fleet.
- **Shared:** `proto/roost/v1/` and `src/gen/roost/v1/` are the source and
  generated contracts; `src/wire/event{,-proto}.ts` own the canonical event
  fold/adapters; `src/cell.ts` owns the grid model. `package.json` is the
  authoritative subpath export map.
