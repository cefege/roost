# Agent chat

The OMP web agent's client contract: `AgentChat*` and `AgentOmp*` Connect RPCs
in `protocol/proto/roost/v1/coordinator.proto`, transcript shapes in
`roost_protocol::wire::agent_chat`, and coordinator/worker admission and projection.
The retained Rust-native contract below is archival, not an active product mode.

Implementation anchors: `crates/roost-coord/src/agent/omp/` (projection and RPCs),
the worker's OMP adapter, and `crates/roost-web/src/components/agent_chat/`.
Native engine code remains under `crates/roost-agent/` with explicit test fixtures.

## Backends and OMP settings

OMP is the only active backend. Normal clients send `AgentChatCreate.backend=omp`;
an omitted creation backend also selects OMP. Explicit native creation and native
execution/configuration/auth mutations reject as archived in production. Native
code, conversation journals, settings and credentials are preserved, not migrated
to OMP or deleted. Production starts no native recovery, schedules or tool wakeups.

Stored `AgentConversation.backend` values retain their compatibility semantics:
`omp` identifies OMP; empty or unknown stored values decode as native. The enum's
Native default is not the creation default and must not relabel old history.
Read-only native summaries remain available to identify archived links. The
normal UI retains that metadata but excludes it from conversation navigation
and mounts no native controls.

OMP conversations run `omp acp --approval-mode yolo` on their fixed worker and cwd.
A worker-owned `--extension` awaits OMP's background and runtime-provider discovery
before ACP publishes its initial choices; ordinary extension discovery remains enabled.
The worker verifies a per-process completion marker before emitting `SessionReady`.
Missing, failed, or malformed discovery status fails initialization visibly.
Credentials stay under that machine's `~/.omp`; Roost distributes no secrets.
OMP slash commands come from ACP `AvailableCommandsUpdate`, including same-session
reload updates; replayed messages are still suppressed. They are inserted into the
composer as OMP commands and are not translated into native agent commands. OMP
permissions and `elicitation.form` requests remain pending until an explicit browser
response; closing, canceling, or ending the ACP prompt cancels unanswered requests.
Startup requests are delivered while initialize, load, new-session, or authentication
is still pending. Stop cancels that startup and its queued prompts; a canceled load
does not fall through to a new session. No terminal-only OMP screen is represented
as an HTML interaction.
Bare `/model` opens the worker-backed model picker. Bare `/plan` toggles between
ACP's offered plan and non-plan modes without starting a turn. `/plan <prompt>`
enters plan mode before submitting an idle, non-plan session's prompt; an already
planning or running session rejects it without submitting or clearing the draft.
The transcript changes only when ACP reports its current mode. Roost-native inspect,
plan-decision, answer, rewind, and fork RPCs remain unsupported for OMP; its
interactive approvals and answers use `AgentOmpRespond`. Withdrawal returns false.

`AgentOmpSettingsGet` returns `OmpSettingsView` JSON; `AgentOmpSettingsSet` upserts one
`OmpScopedSettings` JSON row. Each nonempty field resolves independently:
conversation → exact `(worker_fp, cwd)` folder → machine → your defaults → OMP's own
configuration. Empty values inherit. Model identifiers split at the first `/`.
Model and thinking overrides apply on the next prompt. The ACP session's offered modes
are separate from config options; `AgentChatConfigure.mode` applies an offered mode to
the live session immediately, and transcript mode changes only on ACP's current-mode
update.

`AgentOmpSessionOpen` initializes or loads the conversation's ACP session without
submitting a prompt or changing its run state. The active chat opens its session
before the first message, so its controls show OMP's actual current model, thinking
level, and selectable options. Opening the picker refreshes inheritance and requests
a fresh OMP child loading the actor-owned saved-session ID: startup rereads OMP's
provider configuration rather than reusing an old in-memory registry. Active turns
finish before this refresh; replayed history is suppressed.
Every committed `Agent` event invalidates the browser's settings cache, including
catalog-only changes with unchanged selected values. Choices come only from ACP;
an unavailable old pin is not inserted into the selectable catalog.
`Agent` events optionally carry `omp_session` (`ready` or `failed` with an error);
omitted session metadata preserves its previous value. A prompt-free initialization
failure does not fail a user message queued behind it. Failures expose retry.

The `omp_acp_v1`, `omp_acp_session_v1`, and `omp_acp_interactions_v1` capabilities
are all required to create, reopen, resume, or prompt an OMP conversation. This prevents
an older ACP client from auto-approving OMP plan proposals when it lacks form elicitation.
Live mode changes and interaction responses also require `omp_acp_interactions_v1`.
An online worker missing a required OMP capability is reported as needing an update,
not as offline.

Worker event sequences are contiguous within an actor's `run_id`; unacknowledged
batches are resent after link snapshot readiness. The coordinator commits
deduplication sequence, transcript, event cursor and metadata atomically, then publishes and
ACKs. Snapshot/event-cursor catch-up works while the worker is offline. Retention is bounded
to 8 MiB per actor and reports output loss explicitly.

Link reconnects preserve OMP processes. Worker restarts fail active turns and resolve
all pending interactions from the old actor run; the next message loads the saved ACP
session without projecting its replayed history a second time.
Deferred commands belong to the actor, not a single startup attempt, so failed
catalog preparation cannot discard the queued first prompt.
Authority: `crates/roost-worker/src/omp/`, `crates/roost-coord/src/agent/omp/`,
`crates/roost-protocol/src/wire/agent_chat/omp.rs`.

### OMP transcript presentation

Assistant text, thoughts, and calls retain event order. Tool IDs identify mutable
entries: partial and final results replace the existing call without appending a
second result row. ACP `name` selects the tool renderer; a human `title` never
overwrites that identity. Returned `rawOutput.content` excludes synthesized command
headers; `rawOutput.details` retains execution and display metadata. Numbered OMP
diffs with supplied old/new file contents normalize into contextual unified diffs.

OMP renders ordinary calls with compact previews visible; only uninterrupted read
calls group into path-first disclosures. Native chat retains its activity groups.
Bash follows the latest output with a viewport-limited visual-row budget and explicit
expansion; searches use head previews, completed writes show six numbered lines, and
completed edits fold at eight hunks/forty body lines. Successful todo calls with a
projected plan share the canonical checklist instead of duplicating raw snapshots.
Detailed todo results preserve OMP's native phases, blockers, and abandoned states
using the shared task types. Flat ACP updates with matching tasks update progress
without flattening those phases or erasing states the flat format cannot express.
Live thinking opens by default; completed thinking folds unless the reader made an
explicit choice. Opening thinking exposes the actual text without a separate tail cap.

Pending forms render after the latest transcript entries, independently of tool
expansion, with an explicit waiting-for-answer state. Single-question prompts are
not repeated as both a message and a field title. OMP's optional custom-answer
editors open on demand; closing one removes its hidden override. String editors
retain newlines and quotes. A false confirmation is a submitted answer, distinct
from decline and cancellation; no choice takes effect before explicit submission.

ACP does not transport OMP's pre-execution streamed tool-argument previews. Roost
renders supplied start arguments and live result snapshots, never simulated argument
updates. Semantic roles and supplied ANSI styles use the existing theme.

Native-only commands, plan review, ownership/recovery, provider and model-role
sections below document the retained implementation and its explicit native test
fixtures. They do not enable those workflows in the production product. Shared
transcript, attachment and request-id rules still apply where the OMP contract
above supports them.

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

`AgentOmpRespond` uses a client-minted `request_id` and is deduplicated in
`omp_inputs` separately from native commands. Its `item_id` must still name a
pending canonical OMP interaction; a stale interaction fails rather than reusing
an ACP request id or implying approval.

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
scrollable document and actions below it. Reaching either boundary of the
document or outline hands scrolling to the main chat; the outer chat still
contains scrolling within the application. Phone layouts omit the outline.
Outline headings use the same Markdown parser options as the rendered plan.
Plan prose uses the shared `MarkdownSize::Sm` typography and normal UI font;
headings stay on the compact agent scale rather than document-scale type.

A proposed plan requires an explicit Approve, Refine or Reject decision before
ordinary chat continues. The composer preserves its draft read-only and offers
**Show plan** to reach the decision controls. Message mutation actions and new
attachments are unavailable while the proposal is pending; targeted refinement
uses the same composer as the exception.

The server returns `failed_precondition` for new ordinary input before appending
it or reserving its request ID. Retrying an already accepted request retains
its original result. Rewind is refused, and run admission and recovery hold
queued work while a proposal awaits a decision. A crashed active run settles
idle at its durable model-consumption boundary without discarding held input.

**Approve in new chat** is the primary action: `AgentChatPlanDecide` with
`decision=approve_new` starts an execution conversation and the client opens
the returned `new_conversation_id`. **Execute in this chat** remains in the
secondary actions menu and sends `decision=approve`.

**Reject** sends `decision=reject`, marks the proposal rejected, exits plan mode
and unlocks ordinary chat. Previously accepted held and queued input resumes
in journal order; without such input, rejection starts no model call.

**Refine** targets the proposal in the normal main composer, focuses it and
preserves its draft. Sending feedback calls `AgentChatPlanDecide` with
`decision=refine`; it is not a normal message or a transcript rewind.
Cancelling refinement preserves the draft and restores the decision lock.
Request failure preserves draft and target. A decided or superseded target
rejects submission until cancelled or replaced; stale feedback must never
silently become an ordinary message.

Refinement and message editing are exclusive. Starting refinement clears a
plain edit but refuses an edit with retained attachments instead of dropping
them. An older request's completion cannot clear a restarted refinement.
Authority: `crates/roost-web/src/components/agent_chat/plan_card.rs`,
`plan_refinement.rs` and `composer_submit.rs`;
`crates/roost-agent/src/admission.rs`, `plan_mode.rs` and `runtime/recovery.rs`;
`crates/roost-coord/src/agent/store_execution.rs`.

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

## Provider endpoints

`experiential-labs` accepts API-key credentials. Chat models use
`/v1/chat/completions`; native classifiers use `/v1/systemone` and belong in
the Judge role, not a conversation's chat-model pin. The built-in Judge chain
tries TypeSafe, Experiential Labs, then OpenRouter.

OpenAI-compatible chat roots ending in `/v1` receive `/chat/completions` once.
Bare endpoint overrides receive `/v1/chat/completions`.

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

The archived Rust-native engine retains its own behavioral and storage contract.
It is not a second active agent option or the source of OMP's provider settings.

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

Archiving does not rewrite operator settings, credentials or conversation
journals. Compatibility with retained journal variants must be checked before
any native reactivation; fresh-database acceptance does not establish that
compatibility.
