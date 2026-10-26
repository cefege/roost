# Agent chat

The built-in agent's client contract: the `AgentChat*` Connect RPCs in
`protocol/proto/roost/v1/coordinator.proto`, the transcript shapes in
`roost_protocol::wire::agent_chat`, and the coordinator-side rules that make
them safe when several coordinator replicas share one database.

Implementation anchors: `crates/roost-coord/src/agent/` (RPC handlers, store),
`crates/roost-agent/src/runtime/` (admission, leases, recovery),
`crates/roost-coord/src/cluster/notify.rs` (cross-replica notices).

## Commands and request ids

The idempotent commands below carry a client-minted `request_id` (at least
1 character; the web client sends 128 random bits as hex):

| RPC | `request_id` field | Retried with the same id |
|---|---|---|
| `AgentChatSubmit` | 3 | applied once; the retry returns OK |
| `AgentChatAbort` | 2 | applied once |
| `AgentChatWithdraw` | 3 | applied once |
| `AgentChatAnswer` | 4 | applied once |
| `AgentChatPlanDecide` | 5 | returns the first decision's `new_conversation_id` |
| `AgentChatRewind` | 4 | after completion, returns the first rewind's `text`; while still applying, returns empty `text` |
| `AgentChatFork` | 3 | returns the conversation the first fork created |

An empty `request_id` is `invalid_argument`. Deduplication is per
`(conversation_id, request_id)` in table `agent_commands`. A retry of a
command whose first attempt failed returns `failed_precondition` with the
first attempt's message; a retry while the first attempt is still applying a
fork returns `unavailable`.

`AgentChatSubmit.when_busy` selects how plain user text is admitted:

- `steer` (also the empty legacy value): append a `User` entry and active
  `RunIntent` atomically with the command row. During a run it joins at the
  next request boundary, not an already-dispatched provider request.
- `follow_up`: while busy, persist an opaque pending `QueuedInput` without
  adding it to active model history. Fenced run completion activates it into
  a `User` entry and recoverable run intent in one transaction.
- `reject`: while busy, return a busy error without appending input or
  reserving the request id. When idle, every mode starts work immediately.

Deduplication precedes the busy check, so an accepted input's retry succeeds
even when the run state changed. The browser persists the logical submission's
text, mode and request id before sending; response-loss retries after reload
reuse that identity. Slash commands retain their separate command admission.

Before a provider request, a fenced `InputConsumed` journal entry claims its
input boundary. `queue_delivered` moves claimed messages from the queue into
user transcript items without duplicating an already-visible user item.
Withdrawal succeeds only before consumption: `withdrawn=true` removes the
input durably and emits `queue_removed`; consumed or already-withdrawn input
returns `false`. Repeating the withdrawal request also returns `false` once
its first attempt removed the input. A false response never claims cancellation.

`AgentChatAbort.background=false` interrupts foreground work but leaves
background jobs running; absent or `true` stops both. Stop is durable: fenced
completion and conditional automatic restart cannot overtake a newer Stop.
A newer explicit input can still start work after Stop.

## Run ownership

Agent work runs only on the coordinator replica that owns the cluster's links
(the `coord_leases` row `cluster:links`, see `crates/roost-coord/src/cluster/`);
every other replica proxies client connections to it unread. Within that, a
conversation's run is executed by exactly one replica, the holder of its row
in `agent_run_leases` — the guard that keeps a run single across an ownership
handover, when the old owner is still draining:

- **Claim.** A replica claims the lease before it writes anything for a run
  (start, recovery, adoption). A lease is free when absent, released
  (`expires_ms = 0`) or expired; the same replica re-claiming its own lease
  bumps `epoch`.
- **Fence.** Every journal append the run makes carries `(owner, epoch)`; an
  append under a superseded epoch fails and the run stops without writing a
  settlement.
- **Renew / release.** A running conversation renews every 10 s (TTL 30 s)
  and releases on settle. A replica that shuts down gracefully releases every
  lease it holds; one that crashes loses them at expiry.
- **Adoption.** Every replica sweeps every 15 s for runs whose lease expired
  without release, or whose row says `running` with no live lease, claims
  them, and resumes them through durable recovery. A tool call whose result
  was never recorded is not dispatched again; it is recorded as an
  outcome-unknown error.

The owner opens one Pi Durable namespace per native conversation. Core batches
and native projection rows settle atomically under the same lease fence and a
core-sequence compare-and-swap; ID allocation is fenced too. `0013` adds these
tables without rewriting older migrations. The indexed in-memory kernel view
is adopted only after the SQL transaction succeeds. Publication failures do not
turn an already committed effect into retryable uncommitted work.


## Cross-replica wake-ups

After a replica admits input, an abort, or deletes a conversation it does not
own, it sends `pg_notify('roost_agent', <conversation_id>)`. The owner
re-reads the journal and hands any input it has not queued to the live run,
or cancels the run when an aborted intent was written after the run started.
Notices are best-effort: the listener resynchronizes every live run after it
reconnects, and the adoption sweep covers a lost owner. On SQLite the
coordinator is the only replica and notices stay in process.

## Transcript events and cursors

`AgentChatSnapshot` returns the current transcript projection and its durable
event cursor. `AgentChatEvents(conversation_id, after_cursor, limit)` returns
event batches strictly after that cursor, in ascending cursor order. A limit
of zero means 500; larger limits are capped at 500. Each batch has one cursor
and a JSON array of `ChatEvent` values.

Clients apply a batch only when its cursor is exactly one greater than the
loaded cursor. A gap marks the transcript stale; clients first request the
missing batches and fall back to a snapshot if batches were pruned or the gap
remains. `truncated` means the requested cursor predates retained history.
Event batches older than 24 hours may be pruned, except that every
conversation's newest batch is retained so its cursor continues after restart.
The cursor and retained batches are read from one database snapshot.

## Native inspection and configuration

`AgentChatInspect` returns `AgentInspection`: native conversation/run state,
context and usage, durable inbox, current activity, offered tool definitions
and effective read-only setting. The coordinator joins runtime state with the
live transcript and worker registries; there is no demo shared-state store.

`AgentChatConfigure.read_only` is persisted per conversation, independently
of Plan mode. Reading tools remain offered; mutating tools and delegation do
not. Dispatch revalidates the owning conversation's policy after an offline
worker wait, then rechecks cancellation before sending a frame. The owning
conversation id is distinct from advisor/loop worker-state namespaces.
Hidden controller context loading and an explicit user's file rewind remain
available; neither is a model-offered tool.

Summary changes use `roost_agent_summary`, separate from run wake-ups, and do
not start model work. A peer reloads the persisted summary; reconnect and lag
repair resynchronize summaries.

Snapshot replay filters queued/user membership against the newer authoritative
journal, including membership inside cached `Reset` payloads. A withdrawal
committed before its removal event was persisted cannot reappear on reconnect.

## Agent scope and storage compatibility

The native Rust agent's behavioral baseline is `d5cf3bda`, immediately before
the full OMP-harness parity project. Exact OMP parity is not this contract.

Applied SQLite and Postgres migration history remains intact. The single
advisor reads and writes the empty `roster_slug` slot through the current
`(conversation_id, roster_slug)` key; named roster slots are inactive.

The engine is the Rust fork recorded in
[`third_party/pi-durable-rs/ROOST-PATCHES.md`](../../third_party/pi-durable-rs/ROOST-PATCHES.md).
It owns generation, tool/nested-tool and compaction tasks. Native commands,
provider accounts, worker dispatch and product policies retain their existing
API ownership. The source pin, namespace identities and compatibility boundary
are documented in [`docs/plans/pi-durable-rust.md`](../../docs/plans/pi-durable-rust.md).

Existing journal history is imported with origin mappings, including effective
rewinds and compaction visibility. Kernel replies are projected once, not
re-imported as new messages. Optional rich tool content and provider data retain
image ordering and replay metadata when materialized native history is forked.
Old tool rows without these fields remain readable.


The rollback does not rewrite operator settings, credentials or conversation
journals. Compatibility with journal variants written by the retired parity
features must be checked before a production rollout; fresh-database acceptance
does not establish that compatibility.
