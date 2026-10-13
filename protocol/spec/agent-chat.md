# Agent chat

The built-in agent's client contract: the `AgentChat*` Connect RPCs in
`protocol/proto/roost/v1/coordinator.proto`, the transcript shapes in
`roost_protocol::wire::agent_chat`, and the coordinator-side rules that make
them safe when several coordinator replicas share one database.

Implementation anchors: `crates/roost-coord/src/agent/` (RPC handlers, store),
`crates/roost-agent/src/runtime/` (admission, leases, recovery),
`crates/roost-coord/src/cluster/notify.rs` (cross-replica notices).

## Commands and request ids

Every command that changes a conversation carries a client-minted
`request_id` (at least 1 character; the web client sends 128 random bits as
hex):

| RPC | `request_id` field | Retried with the same id |
|---|---|---|
| `AgentChatSubmit` | 3 | applied once; the retry returns OK |
| `AgentChatAbort` | 2 | applied once |
| `AgentChatAnswer` | 4 | applied once |
| `AgentChatPlanDecide` | 5 | returns the first decision's `new_conversation_id` |
| `AgentChatRewind` | 4 | returns the first rewind's `text` |
| `AgentChatFork` | 3 | returns the conversation the first fork created |

An empty `request_id` is `invalid_argument`. Deduplication is per
`(conversation_id, request_id)` in table `agent_commands`. A retry of a
command whose first attempt failed returns `failed_precondition` with the
first attempt's message; a retry while the first attempt is still applying a
fork returns `unavailable`.

Plain user text is admitted atomically: the `User` entry, an active
`RunIntent` through it, and the `agent_commands` row commit in one
transaction. A slash command records its request row first and then runs.

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
