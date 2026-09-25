<!-- AUDIENCE: claude -->
<!-- Worker map: root holds process primitives and keeper-closure pins; explicit domain folders own behavior. -->
<!-- Protocol meaning is authoritative under protocol/spec; this README records worker ownership and local seams. -->

# @roost/worker

The Bun process on each released macOS/Linux fleet machine. It owns every session shell PTY, the authoritative terminal grid, the keeper subprocess, local loopback door, and outbound coordinator link. Agent CLIs are ordinary programs inside PTYs; the worker observes and fences them but does not own agent conversations or approvals. The paused Windows implementation remains in `apps/roost-cli` and is not a worker release claim.

The coordinator remains the authorization and durable-control authority. Direct terminal and attachment carriers are bounded client/worker paths, not replacements for CoordLink or Sync metadata/control. Protocol contract index: [`protocol/README.md`](../../protocol/README.md). Normative carrier and transport behavior: [`protocol/spec/worker-link.md`](../../protocol/spec/worker-link.md), [`protocol/spec/direct-terminal.md`](../../protocol/spec/direct-terminal.md), [`protocol/spec/terminal-stream.md`](../../protocol/spec/terminal-stream.md), [`protocol/spec/attachments.md`](../../protocol/spec/attachments.md), [`protocol/spec/agent-metadata.md`](../../protocol/spec/agent-metadata.md), and [`protocol/spec/session-events.md`](../../protocol/spec/session-events.md).

## Entry point

`apps/worker/src/main.ts` exports `runWorker()` and the boot-admission seam. Boot loads config and key material, installs when required, starts the local terminal door before CoordLink, opens the durable session-event store, constructs coordinator-link dependencies, performs reconciliation, starts session/health owners, and only then admits keeper survivors. SIGTERM/SIGINT close long-lived owners and the event store but leave the keeper alive.

`apps/worker/src/transport/coord-link-deps.ts` is the single dependency assembly for coordinator callbacks. `apps/worker/src/boot/boot-local-terminal.ts` composes the one view/grant/input/socket/peer owner set around the worker process epoch. `apps/worker/src/snapshot.ts` is activated only after durable replay/admission has completed.

## Module map

One row per current owned source or test directory. Root is limited to process primitives and the keeper-closure files whose paths and import specifiers are compatibility-sensitive.

| Directory | Owns | Must not own |
| --- | --- | --- |
| `apps/worker/src/` | Process entry/FSM, file RPCs, durable snapshot activation, shell specification, and the scrollback ring pinned by keeper bundle identity. | Domain behavior, protocol schema definitions, coordinator handlers, or a second keeper/session owner. |
| `apps/worker/src/agents/` | PID-attested agent observation, process scans, registry/reporting, prompt and private-reference recovery, stable detection, and integration installation. | Coordinator agent handlers, UI notifications, or public conversation transcripts. |
| `apps/worker/src/agents/integrations/` | Integration asset manifests, installation transaction/proof, and environment setup. | Agent status identity or browser pairing state. |
| `apps/worker/src/agents/integrations/omp/` | OMP integration-specific generated/install assets. | Cross-integration policy or session lifecycle. |
| `apps/worker/src/agents/integrations/pi/` | Pi integration-specific generated/install assets. | Cross-integration policy or session lifecycle. |
| `apps/worker/src/attachments/` | Attachment grants, upload/file store and journal, direct/peer transfer ports, packet budgets, reaping, and local/browser command bridges. | Terminal packet or grant state, session lifecycle, or protocol schema definitions. |
| `apps/worker/src/boot/` | Worker startup and reconciliation, keeper/local-terminal composition, session replay, and keeper-admission seams. | Long-lived domain state or a second session/keeper owner. |
| `apps/worker/src/browser-commands/` | Browser-originated file, diagnostic, and terminal command parsing, dispatch, spawn, and capture boundaries. | Coordinator-link state, local-door transport, or keeper/session mutation policy. |
| `apps/worker/src/host/` | Worker config/JWT, service definition and install policy, listeners/identity, Git/PR sampling, and host metrics. | Coordinator, session, or terminal behavior; browser-safe platform policy. |
| `apps/worker/src/local-door/` | Loopback UI server, local bootstrap and terminal sockets, grants, input/delivery/control, scrollback, and pre-hello admission. | WebRTC peer state, attachment state, or non-loopback listeners. |
| `apps/worker/src/session/` | Session records and lifecycle, spawn/resume/respawn, metadata/control/event emission, resize capture, and authoritative cell sinks. | Keeper PTY hosting, coordinator projections, or a second terminal view/grid owner. |
| `apps/worker/src/terminal/` | Terminal pipeline snapshots/bounds, stream scan/query/replay, input-route ownership, work budgets, and core capacity. | Session lifecycle, keeper transport, or peer/search/view ownership. |
| `apps/worker/src/terminal/peer/` | WebRTC terminal connection/owner, native loader, packet ports/budgets, request validation, and history reservation. | Coordinator grant policy, loopback sockets, or session/grid ownership. |
| `apps/worker/src/terminal/search/` | Incremental terminal search matching, batching, cancellation, results, and scheduling. | Session storage, terminal rendering, or peer transport. |
| `apps/worker/src/terminal/view/` | Authoritative view membership/geometry ownership, screen projection, and stream renewal. | Session lifecycle, protocol schema definitions, or direct-peer packet state. |
| `apps/worker/src/transport/` | Outbound CoordLink FSM, codec/types/constants, reconnect/backoff, downstream dispatch, outbox/native writer, agent-status/metadata lanes, and durable `SessionEventStore`. | Browser route election, local direct carrier frames, or protocol message definitions. |
| `apps/worker/src/keeper/` | Multiplexed keeper protocol/envelope/IO/terminal modules, worker pool, PTY frame handling, history/input queues, probe/stamp, and keeper update admission. | Coordinator write-gate policy, browser UI, or session event publication semantics. |
| `apps/worker/src/diag/` | Opt-in terminal incident capture, byte capture, recorder/pools, evidence, ACK, bundle writer, and secure capture storage. | Ordinary terminal frames, public diagnostics, or a second capture state owner. |
| `apps/worker/src/util/` | Monotonic clock and worker-native path helpers. | Browser-safe path policy from `@roost/platform` or protocol schemas. |
| `apps/worker/tests/` | Recursive Bun suites mirroring worker source domains for keeper, transport, direct carriers, sessions, agents, attachments, and diagnostics. | Coordinator server or browser component tests. |

## Invariants

- `apps/worker/src/transport/coord-link-unacked.ts` drives the durable barrier: hello, one-at-a-time replay/ACK, snapshot, then live. The event store reserves capacity before mutation and deletes only after exact ACK.
- `apps/worker/src/keeper/` is the only PTY host. Its frame `payload` views are valid only until the next read; any retained or queued payload must be copied. A worker restart probes and adopts a compatible survivor, then resumes channels.
- Worker files in the keeper bundle closure—`apps/worker/src/session-scrollback-ring.ts`, `apps/worker/src/shell-spec.ts`, and `apps/worker/src/keeper/`—keep their paths and import specifiers byte-stable. Bun's minified identifier assignment includes import-specifier text, so moving one changes the implementation digest and makes running keepers unadoptable by the new worker.
- `apps/worker/src/local-door/local-ui-server.ts` is loopback-only and serves the local bootstrap and local-terminal socket. A non-loopback bind or foreign `Host` fails closed. Local grants are digests in memory and require a fresh coordinator grant after restart.
- `LocalTerminalSockets` is the one local-terminal frame boundary. Loopback and WebRTC share its validated proto frames, while each carrier owns only packet admission, backpressure, and close.
- Direct input is fenced by `TerminalInputRouteOwner` immediately before keeper input. Started work is not replayed; ambiguous outcomes are not retried.
- `TerminalViewOwner` and `session-cell-sinks` remain the authoritative view/cell owners. Direct ports stage frames but cannot create a parallel session, view, input, or packet implementation.
- Attachments have their own grants, leases, byte destination, capacity buckets, and packet framing. They never share terminal grant or packet state.
- `src/agents/` reports identity with the complete `(status_epoch, occupant_id, source)` triple. A retired occupant's completion remains active until acknowledged; private conversation references remain worker-only recovery metadata.
- `packages/protocol` supplies the worker transport, local-terminal, cell, peer, and attachment shapes. This README intentionally leaves their normative limits and state machines in the linked protocol specs.
- The worker uses structured logging through `@roost/observability/log`; it does not interpret agent output or own a second public agent model.
