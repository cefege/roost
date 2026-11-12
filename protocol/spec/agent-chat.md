# Agent chat

The OMP web agent's client contract: `AgentChat*` and `AgentOmp*` Connect RPCs
in `protocol/proto/roost/v1/coordinator.proto`, transcript shapes in
`roost_protocol::wire::agent_chat`, and coordinator/worker admission and projection.
OMP is the only backend; the Rust-native engine and its contract are gone.

Implementation anchors: `crates/roost-coord/src/agent/omp/` (projection and RPCs),
the worker's OMP adapter, and `crates/roost-web/src/components/agent_chat/`.

## Backends and OMP settings

OMP is the only backend. Normal clients send `AgentChatCreate.backend=omp`; an
omitted creation backend also selects OMP, and any other value is
`invalid_argument`. Stored native conversation rows are inert: no RPC lists,
reads, resumes or mutates them, and no native recovery, schedule or tool wakeup
runs.

Stored `AgentConversation.backend` values retain their compatibility semantics:
`omp` identifies OMP; empty or unknown stored values decode as native. The enum's
Native default is not the creation default and must not relabel old history.

OMP conversations run `omp acp --approval-mode yolo` on their assigned worker and current cwd.
A worker-owned `--extension` awaits OMP's background and runtime-provider discovery
before ACP publishes its initial choices; ordinary extension discovery remains enabled.
The worker verifies a per-process completion marker before emitting `SessionReady`.
Missing, failed, or malformed discovery status fails initialization visibly.
Credentials stay under that machine's `~/.omp`; Roost distributes no secrets.
OMP exposes its existing interactive controllers through the versioned TSP command
UI extension (`agentCapabilities._meta.commandUi = {version:1,protocol:"tsp"}`).
`AgentOmpUi` carries `open`, `event`, `input`, `command`, `close`, and `host_reply`;
the worker supplies the canonical ACP session identity. `/models`, `/resume`, and
`/settings` are OMP's actual terminal workflows, not Roost-authored forms. Session
switches update the saved resume identity, cwd and canonical history.

`OmpUiDocument` is the only TSP fold. Frames apply atomically against contiguous
surface sequences; a rejected operation rolls back all touched nodes, properties,
catalogue rows and indexes. A null `set` value removes a property. Picker
`itemsAdd`/`itemsDel` arrays mutate the retained catalogue; clearing those transient
props with null does not undo their entries. Bounds: 2 MiB per message, 32 MiB per
document, 20,000 nodes, 128 levels and 128 surfaces. Opening a view does not submit
a prompt. Adoption/reconnect rebuilds a complete document before deltas resume.

HTML renders the supplied semantic nodes with stock UI components. Nonmodal
autocomplete leaves its editor active; modal sheets and screen surfaces own focus
and cover lower layers. Query strings, selection, role assignments and settings
remain OMP-owned. Pointer list events address the list and preserve the clicked
wire ID for OMP's key decoder. Browser text uses one acknowledged edit in flight;
typed-ahead drafts and UTF-16 carets survive delayed frames and complete submits.

Private host services and receipts are fenced by conversation, authenticated
device, tab, view and worker-owned transport generation. Clipboard text and URLs
never enter transcript events or logs. Browser permission refusal leaves a real
Copy/Open action pending; only a confirmed effect is acknowledged. Host-service
input/event success receipts wait for durable semantic frames; `receipt=false`
success stays silent, but an asynchronous host rejection still reports a private
failure. Closing/replacing a view cancels its pending effects.
`/quit` detaches the semantic view without deleting its conversation or stopping
the worker. The pane offers an explicit reopen action, which reconstructs a fresh
transport and document. Viewport callbacks and teardown read that current identity.

Stats dashboards use short-lived `/omp-stats/<capability>/` routes bound to that
same origin tuple. The worker pins a locally assigned address, refuses redirects
and forwards allowlisted headers without credentials. The dashboard runs in an
opaque-origin CSP sandbox. Each capability permits eight outstanding normal
requests (30 s total including upload), two SSE streams and bounded request rate.
Authority: `roost-protocol::native_dashboard`, `roost-coord::http::native_dashboard`
and `roost-worker::browser_commands::native_dashboard`.

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
level, and selectable options. The semantic command UI keeps those controllers
and the managed AgentSession together; model and provider changes follow OMP's
own refresh and persistence rules rather than rebuilding a second browser catalog.
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

The active chat projects OMP's TSP document, including its transcript, composer,
tool previews, disclosure headers and command sheets. Declared preview windows
select borrowed source nodes; expanding a disclosure reveals the complete body.
Copy actions retain complete source text even when its paint is clamped.
Thinking headers that reference child IDs resolve to those children and are not
duplicated in the body. Native reveal/scroll requests remain surface-local,
bounded and transient; applying one never substitutes a second selection model.

ACP transcript events remain durable for snapshots, history and nonvisual clients.
Tool IDs identify mutable entries; returned raw-output details preserve execution
metadata. Native plan/answer/permission prompts stay owned by OMP's controllers;
standard ACP permissions and elicitation retain their existing explicit-response
channels.

`/resume` projects OMP's session picker: scope-aware search placeholder, day
groups without a query and flat ranking with one, `itemsAdd` badge upserts that
stick until the listing replaces `items`, and the action row Resume (enter),
Delete (backspace), Rename (f2), Pin/Unpin, scope toggle (tab) and Close (esc).
Delete over an empty caret side belongs to the program: the browser forwards
Backspace/Delete only when the caret has text to edit on that side. The delete
confirmation is the picker's confirm strip; it captures its session when
requested, its button confirms, and Esc cancels. Resuming switches the saved
resume identity, cwd and canonical history; a cold open or `/restart`
reconstructs the transcript from the saved session.

`/btw` projects OMP's side-question history sheet: per-turn question, time,
status and error, Close (escape) and Follow up (f). The follow-up composer
opens inside the sheet with Cancel (escape) and Send (enter); a sent follow-up
appends its own turn under the record. Inject/steering controls appear only
while OMP's inject setting allows and a turn can accept them; idle surfaces on
both terminal and browser show none.

## Commands and request ids

The idempotent commands below carry a client-minted `request_id` (at least
1 character; the web client sends 128 random bits as hex):

| RPC | `request_id` field | Retried with the same id |
|---|---|---|
| `AgentChatSubmit` | 3 | applied once; the retry returns OK |
| `AgentChatAbort` | 2 | applied once |
| `AgentChatWithdraw` | 3 | applied once |

An unknown conversation returns `not_found`.

`AgentOmpRespond` uses a client-minted `request_id` and is deduplicated in
`omp_inputs`. Its `item_id` must still name a pending canonical OMP interaction;
a stale interaction fails rather than reusing an ACP request id or implying
approval.

An empty `request_id` is `invalid_argument`. Deduplication is per
`(conversation_id, request_id)` in table `agent_commands`. A retry of a
command whose first attempt failed returns `failed_precondition` with the
first attempt's message.

`AgentChatSubmit.when_busy` selects how plain user text is admitted while a
turn runs: `reject` returns a busy error without reserving the request id;
`steer`, `follow_up` and the empty legacy value queue the prompt behind the
running turn. When idle, every mode starts work immediately.

Deduplication precedes the busy check, so an accepted input's retry succeeds
even when the run state changed. The browser persists the logical submission's
text, busy mode, ordered attachment upload IDs and request ID before sending;
an unchanged retry reuses that identity. Changing an attachment starts a new
logical submission. Slash commands retain their separate command admission.

Withdrawal succeeds only before the prompt is consumed: `withdrawn=true`
removes the queued input durably; consumed or already-withdrawn input returns
`false`. A false response never claims cancellation. OMP conversations always
answer `false`, because their queue lives in OMP's own session.

`AgentChatAbort` stops the running turn. Stop is durable: a completion that
loses the race cannot overtake a newer Stop. A newer explicit input can still
start work after Stop.

## Attachments

`AgentChatSubmit.attachments` carries ordered `AgentChatAttachment` values:
`filename`, `mime_type`, inline `data` bytes and a completed worker `path`.
Images require nonempty data; other files require a nonempty worker path.
An attachment-only submit is valid. The worker receives the uploaded path in the
prompt text; the transcript keeps the user text raw and shows
`attachment_names` separately.

Picker, drop and file paste use the existing worker upload queue and transfer
receipts. The composer blocks submission until every staged upload completes.
Failures retain the draft and files; success removes only the submitted upload
IDs, preserving files staged while the request was pending.

Transfer authorization accepts an open terminal session or an existing OMP
conversation. Conversation resolution reads the conversation's owning worker and
nonempty working directory, then requires a registered, routable worker; an
unknown conversation is `not_found` and never reaches a worker. The immutable
upload descriptor and exact device/tab checks are unchanged.
Attachment-specific `AttachmentTargetId` accepts terminal UUIDs or `c-` plus
24 lowercase hexadecimal digits; terminal `SessionId` stays UUID-strict.
Authority: `crates/roost-coord/src/attachments/conversation_target.rs`,
`crates/roost-web/src/components/agent_chat/composer_submit.rs`.

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

## Provider settings

The coordinator serves no provider surface: there is no model catalog, account
list, credential store, login flow, usage report or model-roles document. Those
RPCs and the `roost-llm` crate are gone rather than stubbed.

An OMP conversation's model, thinking level, provider and credentials come from
the worker's own OMP configuration under `~/.omp`. Roost distributes no secrets
and reads no provider quota.

## Agent scope and storage compatibility

OMP is the only supported agent: its fork retains the terminal TUI and exposes
the same controllers and content to HTML. A/B checks against the TUI govern web
parity, alongside native HTML selection, accessibility, clipboard and input.

Worker initialization requires `_meta.commandUi = {version: 1, protocol: "tsp"}`.
A missing or incompatible capability fails before opening an OMP session or
emitting `SessionReady`; an unpatched CLI is not a plain-chat fallback.

Applied SQLite and Postgres migration history remains intact, including the
native conversation, journal, schedule and advisor tables. Those rows are inert:
nothing lists, reads, resumes or mutates them. Deleting them is a separate
operator decision, not a migration.
Snapshot and event catch-up both return `not_found: conversation not found` for
archived or missing IDs. Retention prunes only active OMP batches, keeping its
age threshold and newest batch; cluster notices cannot relay archived events.

Alternative Rust-native/Rpi engines, provider adapters and local tool runtimes
are removed from the source tree and dependency graph. There is no second backend.
