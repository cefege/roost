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
| `AgentChatRewind` | 4 | after completion, returns the first rewind's `text` and original `attachments`; while still applying, both are empty |
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
text, busy mode, ordered attachment upload IDs and request ID before sending;
an unchanged retry reuses that identity. Changing an attachment starts a new
logical submission. Slash commands retain their separate command admission.

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

## Plan review

The web review panel keeps section navigation beside a bounded, independently
scrollable document and actions below it. Phone layouts omit the outline.
Outline headings use the same Markdown parser options as the rendered plan.

**Approve in new chat** is the primary action: `AgentChatPlanDecide` with
`decision=approve_new` starts an execution conversation and the client opens
the returned `new_conversation_id`. **Execute in this chat** remains in the
secondary actions menu and sends `decision=approve`.

**Refine** targets the proposal in the normal main composer, focuses it and
preserves its draft. Sending feedback calls `AgentChatPlanDecide` with
`decision=refine`; it is not a normal message or a transcript rewind.
Cancellation preserves the draft. Request failure preserves both draft and
target. A decided or superseded target rejects submission until cancelled or
replaced; stale feedback must never silently become an ordinary message.

Refinement and message editing are exclusive. Starting refinement clears a
plain edit but refuses an edit with retained attachments instead of dropping
them. An older request's completion cannot clear a restarted refinement.
Authority: `crates/roost-web/src/components/agent_chat/plan_card.rs`,
`plan_refinement.rs` and `composer_submit.rs`.

## Attachments

`AgentChatSubmit.attachments` carries ordered `AgentChatAttachment` values:
`filename`, `mime_type`, inline `data` bytes and a completed worker `path`.
Images require nonempty data; other files require a nonempty worker path.
An attachment-only submit is valid. Non-image paths are synthesized once in
model history; image-capable models receive image blocks. Journal and transcript
user text stays raw, with `attachment_names` displayed separately. A model
without image input receives an explicit attached-image reference.

Picker, drop and file paste use the existing worker upload queue and transfer
receipts. The composer blocks submission until every staged upload completes.
Failures retain the draft and files; success removes only the submitted upload
IDs, preserving files staged while the request was pending. User and follow-up
journal entries retain attachments through activation, recovery and rewind.

Transfer authorization accepts an open terminal session or an existing agent
conversation. Conversation resolution uses the runtime's authoritative worker
and nonempty working directory, then requires a registered, routable worker.
The immutable upload descriptor and exact device/tab checks are unchanged.
Attachment-specific `AttachmentTargetId` accepts terminal UUIDs or `c-` plus
24 lowercase hexadecimal digits; terminal `SessionId` stays UUID-strict.
Authority: `crates/roost-coord/src/attachments/conversation_target.rs`,
`crates/roost-agent/src/runtime/api/attachments.rs`,
`crates/roost-web/src/components/agent_chat/composer_submit.rs`.

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

## Scheduled messages

`/schedule in|every <duration> <text>`, `/schedule at <RFC3339> <text>` and
`/schedule cancel <id>` are coordinator-run commands; bare `/schedule` lists.
Rows live in `agent_schedules` (migration `0014`), keyed by conversation and an
`s-`-prefixed id; `Transcript.schedules` projects them and `ScheduleSet` /
`ScheduleRemoved` events keep clients current. Limits: text 1–4000 characters,
`every` at least 10 minutes, one-shots at most 366 days ahead and not in the
past, 50 rows per conversation. Forks copy no schedules; deleting a
conversation deletes them.

Every replica sweeps due rows every 5 seconds. A sweep submits first, with the
request id `sched-<id>-<due_ms>`, then advances the row with a compare on its
old `due_ms`; request-id dedup makes a replayed submit a duplicate, so each due
instant is delivered exactly once across restarts and replicas. A recurring
row advances by whole steps to the first future instant, never bursting after
downtime. A failed submit leaves the row due for the next sweep.

Editing an earlier user message is a client composition: `AgentChatRewind` to
that message without restoring files, then `AgentChatSubmit` with the edited
text and the original attachments plus any newly staged uploads. `/retry` and UI
Retry resend the authoritative original text and attachments. Rewind command
receipts retain their raw-text format; attachment bytes are reloaded from the
immutable original journal entry rather than duplicated into command receipts.

Rewinding to the composer retains the authoritative original attachments. A
later composer submission places those attachments before newly staged uploads
and identifies each retained attachment in pending submission state by its
immutable journal item ID and attachment index. If the submit receipt is lost,
retrying the unchanged draft reuses the same request ID and attachment identities.

A successful rewind emits a canonical `ChatEvent::Reset` before returning.
Already-open subscribers remove hidden turns before a retry or edited
replacement arrives; refreshing only the initiating browser is insufficient.

## Model roles

`AgentSettingsView.model_roles`, each preset in `model_presets`, and each
`folder_model_roles` row key roles by these names: `default`, `smol`, `slow`,
`plan`, `task`, `tiny`, `judge`, `advisor`, `web`. A value is a comma-separated
selector chain; the first entry that resolves is used. The UI names are Default
model (`default`), Fast model (`smol`), Plan mode (`plan`), General helpers
(`task`), Reviewer helpers (`slow`), Side questions & summaries (`tiny`), Judge
(`judge`), Advisor model (`advisor`) and Search engine order (`web`). The UI
shows Default and Fast first; the rest are advanced overrides.

Native fallbacks (`roost-agent`):

- A subagent role uses its own chain only when that chain is set. Otherwise it
  uses the parent's explicit model pin, then `default`.
- `tiny` uses `smol` when set, else the conversation chain.
- `advisor` uses `slow` when set, else `default`.
- `judge` accepts classifier or chat models; `web` chains name search engines.
  The role picker offers classifiers only for Judge. The native set-model path
  rejects a classifier as a conversation's model.

## Account limits and chat usage

`AgentUsageGet` returns account-wide provider quota windows and marks the pool's
next preferred credential when `conversation_id` and `provider` are supplied.
The selection preserves a usable sticky account and excludes disabled or blocked
credentials; it is not attribution of this chat's previous requests.

Anthropic reports `5h` and `7d` windows. Codex window durations come from
`limit_window_seconds`: an explicit five-hour or seven-day duration uses those
labels, while unrecognized or absent durations retain `primary`/`secondary`.
Missing utilization is unknown, never zero. A failed refresh may retain prior
windows with a last-known note; the UI exposes that note.

The composer ring uses the selected account's explicit weekly window when one
is known, otherwise its most-used known window. Its popover lists each account's
windows, resets and readiness. This chat's recorded input/output tokens and
estimated model cost come from `AgentChatInspect`; neither is presented as a
share of the provider's quota.

Authority: `crates/roost-llm/src/usage.rs`, `crates/roost-llm/src/pool/rank.rs`,
`crates/roost-coord/src/agent/rpc_accounts.rs`,
`crates/roost-web/src/components/agent_chat/provider_usage.rs`.

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
