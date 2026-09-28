worker: local terminal door — grants, prehello, direct sockets and scrollback

Lands `crate::local_terminal`, the worker's direct terminal path: the
coordinator-installed grant store (SHA-256 digests only, active expiry,
epoch fencing), the socket owner that admits loopback and WebRTC packet
ports against it (Hello admission, peer tuple match, grant replacement,
live per-session authorization re-evaluated at every keeper boundary),
direct input/route-claim/probe/history controls, cell and view delivery
through the shared TerminalViewOwner, and the direct scrollback reader
over the one shared page reader. `local_door.rs` is rewritten to v2's
pre-hello owner (deadline, established cap, grant replacement); grants
live in their own store as in v2.

v2 ported: apps/worker/src/local-door/{local-terminal-socket,
local-terminal-socket-authority,-controls,-delivery,-input,
local-terminal-scrollback,local-terminal-grants,local-terminal-prehello,
local-ui-terminal-socket}.ts, boot/boot-local-terminal.ts (owner half),
terminal/peer/terminal-packet-port.ts, and the localTerminalGrant /
localTerminalGrantRevoke cases of transport/coord-link-downstream.ts.

Downstream arms replaced:
- local-terminal-grant: was rpc-error "local terminal grants unsupported
  by this worker" → LocalTerminalGrantPort::install_grant (rpc-ok
  {grant_id} / rpc-error <store refusal>); the absent reply stays only for
  a Dispatcher built without owners.
- local-terminal-grant-revoke: was the no-frame warn arm → 
  LocalTerminalGrantPort::revoke_device (routes, then grants, whose
  removal closes the device's sockets).

Consumers: DownstreamOwners.local_terminal is read by
runtime::downstream::local_grants; WorkerOwners.local_terminal by
runtime::owners (door router: LoopbackOwner::terminal(sockets()), peer
DirectTerminal); SessionTable::session_of_channel by
local_terminal::delivery::PortViewTransport; retained_grid::row_cells by
local_terminal::scrollback and the browser page reader.

Shared/cross-owner edits: link_ports.rs (LocalTerminalGrantPort + field),
runtime/owners.rs (door composition, shutdown dispose first),
runtime/boot_sequence.rs (fingerprint arg), runtime/downstream/mod.rs
(2 arms + mod), session/table.rs (session_of_channel),
session/retained_grid.rs (row_cells), scrollback_read.rs (walk_page asks
continue_read only before a slice with rows, v2
browser-command-terminal.ts:115-117), Cargo.toml dev tokio test-util,
tests/link_downstream_support (fake port), tests/link_downstream_absent
(dropped the two assertions that pinned the replaced absent-owner arms).
Deleted tests/local_door.rs: it pinned the pre-v2 paraphrased API;
replaced by ported local_terminal_prehello + local_terminal_grants.

Mutations: see WDoorTerm report (each guarded product line mutated, its
test seen failing, reverted).
