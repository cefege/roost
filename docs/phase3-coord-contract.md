# Phase 3 — the coordinator's contract

The coordinator is the one component every other component talks to. A worker
dials it for state; a browser calls it for control and reads from it for
truth; a device authenticates to it. This document is the contract that has to
hold for those three to keep working, transcribed from the v2 TypeScript and the
`protocol/spec/` documents.

**How to read it.** Every bound, every close code, and every ordering rule
carries the reason it is that number and a `path:line` citation. A constant
whose reason nobody can find is a constant nobody will dare to change, and
several of the numbers below look arbitrary until you know what they cost.
Where the v2 code and a `protocol/spec/` document disagree, the disagreement is
recorded in **§9 Spec-versus-code disagreements** rather than silently resolved,
because a spec that quietly disagrees with its implementation is a spec that
will eventually be believed over the code.

**Authority.** The written contracts in `protocol/spec/` win over the
TypeScript, which wins over any inherited habit. The wire is fixed by
`protocol/proto/roost/v1/*.proto` and must stay byte-exact through Phase 6 so
that any mix of Rust and TypeScript components interoperates in tests.

**Companion artifact.** The squashed schema lives in
`crates/roost-coord/migrations/0001_init.sql`. That file is generated, and its
header says how; §2 is the prose version of the same facts.

---

## Contents

1. [What the coordinator is, and what it is not](#1-what-the-coordinator-is-and-what-it-is-not)
2. [Schema and migrations](#2-schema-and-migrations)
3. [The durable session-event store](#3-the-durable-session-event-store)
4. [Auth: the credential, the principal, and the boot invariant](#4-auth-the-credential-the-principal-and-the-boot-invariant)
5. [The RPC router](#5-the-rpc-router)
6. [The HTTP listener](#6-the-http-listener)
7. [The worker WebSocket](#7-the-worker-websocket)
8. [The Sync WebSocket](#8-the-sync-websocket)
9. [Spec-versus-code disagreements](#9-spec-versus-code-disagreements)
10. [What could not be determined](#10-what-could-not-be-determined)
11. [The file map](#11-the-file-map)

---

## 1. What the coordinator is, and what it is not

It is:

- the single writer of the SQLite state a fleet agrees on;
- the only place a `SessionEvent` becomes durable;
- the only thing that decides which worker runs what — **the browser never
  dials a worker** (`crates/roost-protocol/src/wire/coord_worker.rs:5-7`);
- the terminator for two long-lived WebSockets and the mount point for the
  Connect RPC surface.

It is **not** the terminal. It stores a `(worker_fp, channel) → session` route
and relays cell frames; it never parses a PTY byte. `crates/roost-coord`'s
manifest says the same: *"Speaks the wire, never the terminal."*

### 1.1 Boot order

Order is not cosmetic here; three of the steps are load-bearing.

| # | Step | Why it is here |
| ---: | --- | --- |
| 1 | resolve and validate the config | A bind that is only unsafe once a proxy is trusted cannot be judged before the rest of the config parses (`crates/roost-host/src/coord_config_loader.rs:3-5`). |
| 2 | open the database, apply pragmas | WAL + a busy timeout must be in force before any migration takes a lock. |
| 3 | pre-migration backup **if the file already existed** | `apps/coord/src/main.ts:66` gates the hook on `existsSync(cfg.dbPath)`. Backing up a file that does not exist is theatre. |
| 4 | run migrations | §2.4. |
| 5 | import `authorized_keys` | Must precede the tenancy invariant: rule 11 below checks freshly imported keys. |
| 6 | **enforce the self-hosted tenant invariant** | *"this is the only tenancy invariant and it must hold before any RPC runs"* (`apps/coord/src/main.ts:82`). A throw here escapes before the listener is constructed, so the coordinator **never binds its port**. |
| 7 | startup janitor, then construct the process singletons | The write gate is *"ONE gate per process: keeper-update exclusivity is meaningless if a mutation path can reach a second instance and bypass the fence"* (`apps/coord/src/main.ts:88-89`). |
| 8 | start the listeners | The listeners must start before maintenance and signal wiring (`apps/coord/src/main.ts:2-3`). |
| 9 | schedule maintenance | Backups, audit retention, pair-request retention (`apps/coord/src/main.ts:243-245`). |

Steps 5 and 6 are ordered against each other on purpose
(`apps/coord/src/main.ts:73-83`).

### 1.2 What the process owns

Every one of these is a **field on a services struct built at boot**, not a
crate-root static. In v2 they are `const` locals in `main.ts` passed down as
dependencies, and the reason is visible in the test surface: v2's unit tests
construct `AnnouncedChannelBarrier` with an explicit `onDrop` callback
(`apps/coord/src/workers/worker-ws-upgrade.ts:21-28`) precisely because there is
no global hub available at construction time. That is a design constraint, not
an accident.

| Owner | Lifetime | v2 site |
| --- | --- | --- |
| the write gate | one per process | `apps/coord/src/main.ts:88-89` |
| the pending-publication store | one per process | `apps/coord/src/main.ts:93` |
| the announced-channel barrier | **one per worker connection** | `apps/coord/src/workers/worker-ws-upgrade.ts:110` |
| the JWT cache and its generations | one per process | `apps/coord/src/main.ts:87` |
| the Sync v2 socket state | one per Sync socket | `apps/coord/src/sync/sync-ws-upgrade.ts:199` |

### 1.3 Shutdown

`apps/coord/src/main.ts:236-252` — idempotent, then: close service health →
stop the server → dispose the coord → detach the terminal view hub → dispose it
→ close the database. v2 ends with `process.exit(0)`; the v3 `serve` function
**returns** instead, because a library that exits the process cannot be called
from a test or from a subcommand.

---

## 2. Schema and migrations

### 2.1 The connection

One SQLite file, one connection, no pool. v2's `openDb`
(`apps/coord/src/db/connection.ts:26-49`) is a single `bun:sqlite` `Database`
behind a Kysely dialect; reads and writes share it. Concurrency comes from WAL
plus a busy timeout plus the in-process write gate. **Modelling a reader/writer
pool would be a behavioural change**, not an optimisation.

The pragmas, with the reason each is set:

| Pragma | Value | Why |
| --- | --- | --- |
| `journal_mode` | `WAL` | *"concurrent readers don't block writers"* (`connection.ts:28`). |
| `busy_timeout` | `5000` | *"prevents SQLITE_BUSY under light write contention"* (`connection.ts:30`). |
| `synchronous` | `NORMAL` | The one that looks wrong and is not: *"WAL + synchronous=NORMAL is SQLite's documented pairing — commits stop fsyncing (only checkpoints do). synchronous=FULL (the default this replaces) put a WAL fsync on the event-loop thread inside `writeAuditLog`, which runs for every `SessionsInput` RPC, i.e. once per keystroke batch; that fsync starves the cell fan-out exactly while the user is typing."* The cost is *"the last transaction(s) on an OS crash or power loss, not a process crash; the `events` table is re-derivable from the worker snapshot every worker emits on reconnect."* (`connection.ts:26-33`). **Do not "fix" this to FULL.** |
| `foreign_keys` | `ON` | Cascade deletes on `workspace_sessions` only work with it (`connection.ts:34`). |
| `cache_size` | `-8000` (8 MiB) | *"explicit rather than the implicit 2 MiB default"* (`connection.ts:36`). Negative means KiB, not pages. |
| `mmap_size` | `0` | *"Mapped pages count against the cgroup; keep them out of RSS"* (`connection.ts:38`). |
| `soft_heap_limit` | `67108864` (64 MiB) | *"backstop on one query's allocations"* (`connection.ts:40`). |
| `wal_autocheckpoint` | `1000` | *"~4 MiB WAL target, pinned so a config change is visible"* (`connection.ts:42`). |
| `journal_size_limit` | `33554432` (32 MiB) | *"Truncate the `-wal` back to 32 MiB after a burst instead of never"* (`connection.ts:44`). |

Every one of these is a load-bearing choice with a written cost. Changing one
without reading its row re-introduces the incident it was written to prevent.

### 2.2 The tables

Twenty-six tables survive. The squashed DDL is
`crates/roost-coord/migrations/0001_init.sql`; the shape that matters:

**Identity and tenancy.** `accounts`, `organizations`,
`organization_memberships`, `dashboards`, `dashboard_memberships`,
`account_identities` (federated logins), `authorized_keys` (the device keys),
`account_devices` (which key belongs to which account),
`authorized_key_revocations`, `password_reset_tokens`, `email_outbox`,
`owner_activation_tokens`, `bootstrap_tokens`, `federated_assertion_redemptions`,
`coordinator_relocation_redemptions`.

**Fleet.** `workers` (the machine registry), `pair_requests` (the pairing
ceremony's in-flight rows).

**Durable state.** `events` (append-only source of truth), `sessions` (the
projection), `workspaces` + `workspace_sessions` (the junction), `tasks`,
`mcp_relays`, `app_settings`, `push_subscriptions`, `audit_log`.

Four v2 tables are **absent by construction**, because v2 created and later
dropped them and the final state is what ships: `agent_entries` (dropped in
0019), and `invitations` / `permission_rules` / `webhook_tokens` with their
indexes (dropped in 0026). A v3 install must not recreate them.

### 2.3 The tenancy triggers — the reason the schema is shaped this way

Tenancy is enforced **in the database**, with 21 `RAISE(ABORT)` triggers, not
in a handler. The columns themselves are nullable; the triggers are the write
boundary.

- `*_require_dashboard_insert` / `*_require_dashboard_update` on `workers`,
  `events`, `sessions`, `workspaces`, `workspace_sessions`, `tasks`,
  `mcp_relays`, `push_subscriptions`: refuse a row whose `dashboard_id` is
  `NULL`. Message: `<table> dashboard scope required`.
- `*_require_scoped_*` and `*_preserve_child_dashboard_update`: refuse a child
  whose scope disagrees with its parent. Messages: `session worker dashboard
  mismatch`, `workspace worker dashboard mismatch`, `workspace session dashboard
  mismatch`, `worker child dashboard mismatch`.
- `authorized_keys_reject_revoked_insert`: refuses re-inserting a revoked key
  (`0018_authorized_key_revocations.sql:10-14`). A revoked key must not come
  back through an `authorized_keys` file re-import.
- `bootstrap_tokens_reject_revoked_minter` (`0024:284-292`).

**Why this is not an application check:** an application check is one a future
code path forgets to call. A trigger is one every path pays. Softening a trigger
is how a session from one dashboard becomes visible in another, and the error
only shows up as someone else's terminal in someone else's sidebar.

The single exception: `bootstrap_tokens.dashboard_id` is `NOT NULL` as a column
constraint (`0024:29`). Everything else is nullable-plus-trigger because v2
needed to backfill NULL scopes in place, and a `NOT NULL` column cannot be
added to a populated table without a rebuild.

### 2.4 The migration runner

v2 wrote its own (`apps/coord/src/db/migrate.ts`) and stores **no checksums**.
Four properties carry over and one hazard does not.

1. **Exact-prefix history.** The applied history must be an exact prefix of the
   known migrations, or boot fails with *"Applied migration history is not an
   exact prefix of embedded migrations: found `<name>` at position `<n>`"*
   (`migrate.ts:279-283`). A retired-slot allowlist (`migrate.ts:266-270`) exists
   because *"Migrations that once shipped and were later removed from the
   embedded set. Their history rows are real — the database DID apply them…
   Dropping one from this table would refuse to boot every coordinator old
   enough to have applied it."*
2. **Ordering is `localeCompare` on the migration stem, not a numeric slot**
   (`migrate.ts:305`). This is not a detail: v2 has **two** `0030_*.sql` files —
   `0030_pair_request_provenance` and `0030_workers_terminal_core_capacity`.
   A Rust port that sorts by slot number applies them in the wrong order and the
   squashed schema comes out different.
3. **One transaction per migration, with the history row inside it**
   (`migrate.ts:337-360`). A partially-applied migration is therefore
   impossible: any failure rolls back and the next boot re-runs the file clean.
4. **Foreign keys are re-verified before each migration and validated after**,
   and `PRAGMA integrity_check` runs on the final pending migration
   (`migrate.ts:293-294, 343-350`).

The hazard: **no checksums**, so an edited `.sql` for an already-applied name is
silently ignored. v3 has one migration, so this is moot for the squashed file —
but any future migration must be a new name, never an edit to `0001_init.sql`.
Editing it would produce a v3 install whose database does not match its
migrations, with nothing to notice.

**v3 runs `sqlx::migrate!` over `crates/roost-coord/migrations/`.** `sqlx`
records applied migrations in `_sqlx_migrations`, which is why
`crates/roost-coord/migrations/0001_init.sql` does not contain v2's `_migrations`
table. The squashed file was produced by replaying all 34 v2 migrations onto an
empty database and dumping `sqlite_master`; it was then verified to be
semantically identical to the replayed history (every table's `PRAGMA
table_info`, `foreign_key_list`, `index_list`, plus every index and trigger's
SQL) and to apply cleanly with `integrity_check = ok` and an empty
`foreign_key_check`.

### 2.5 Backup, snapshot, and export

`createSqliteSnapshot` (`apps/coord/src/db/snapshot.ts:13-46`) is the ordered
procedure, and the order is the point:

1. remove any previous destination — *"never inherit a previous file"*;
2. `VACUUM INTO` — **via `query()`, never `prepare()`**: *"a prepare()d
   Statement is finalized only on GC, so it keeps the source handle busy — the
   coordinator's own `close(true)` then reports 'database is locked' after any
   snapshot. query() is cache-owned."* (`snapshot.ts:16-19`)
3. `chmod 0600`;
4. reopen the **copy** read-only and `PRAGMA integrity_check`; anything other
   than `ok` throws `SQLite snapshot integrity_check failed: …` (`snapshot.ts:24-26`);
5. stream SHA-256 in 1 MiB chunks and return `{size, sha256}`.

Any throw removes the destination: *"A failed run removes destPath — partial
snapshots must never escape to the caller."* (`snapshot.ts:5`)

The nightly backup (`db/backup.ts:44-88`) then gzips that into
`backups/coord_v2.<tag>.db.gz` (dir mode `0700`) via **tmp file then rename**,
which is the atomic publish: a reader never sees a half-written archive under
its real name. `MAX_BACKUPS = 14`; the prune runs only after a successful write
and a prune failure does **not** fail the backup (`backup.ts:74-86`). The
uncompressed intermediate never outlives the call (`backup.ts:70-72`).

`/api/db-export` is the same snapshot with two differences: it sweeps first
*"so the keep count leaves it room"* (`bun-coordinator-listeners.ts:215-221`),
and it arms an unref'd 15-minute timer to unlink the file, because the
listener reads the body **lazily, after the handler returned**
(`bun-coordinator-listeners.ts:39-43`). The resident cap is **2** files, and the
reason is disk, not tidiness: *"Each one is a full copy of the database, so an
age bound alone lets N exports inside a single TTL window pin N times the
database size on the small disks this ceiling exists for."*
Unlinking a two-exports-old snapshot cannot break its download — the fd is
already open and POSIX keeps the inode alive — and Windows refuses the unlink,
which the per-file guard degrades to the age bound.

### 2.6 Audit retention

- Window: `auditRetentionDays`, **default 90**,
  `ROOST_COORDINATOR_AUDIT_RETENTION_DAYS`. `audit_log.ts` is epoch
  **milliseconds** (`audit-retention.ts:100`).
- What is deleted: exactly one method, `SessionsInput`
  (`audit-retention.ts:46-48`). The allowlist is a list of names, not a
  predicate and not a wildcard, and the extension rule is explicit: *"To
  extend: add a method name here. Never add anything authorization, pairing,
  deletion or lifecycle-related, and never replace this with a wildcard."*
  (`audit-retention.ts:36-38`)
- What is **never** deleted: `PairApprove`, `PairConfirm`, `AuthRedeemBrowser`,
  `WorkersDelete`, `WorkspacesDelete`, `SessionsKill`, `SessionsSpawn` —
  *"when was this device authorised, and by whom"* has to survive a year, and
  session lifecycle is low volume (`audit-retention.ts:21-45`).
- **No `VACUUM`**: *"Reclaiming the freed pages needs an EXCLUSIVE lock over the
  whole file and rewrites it end to end — on a live coord that stalls every
  RPC… Shrinking it on disk is a manual, out-of-hours operation."*
  (`audit-retention.ts:141-146`)
- Batches of 10,000 with an explicit `await Bun.sleep(0)` between statements:
  *"bun:sqlite is synchronous and the interceptor's audit inserts run on this
  same thread, so a tight loop over a multi-million-row backlog would block
  every RPC until it finished — the batching alone buys nothing without an
  explicit yield."* (`audit-retention.ts:132-136`)

The motivating incident, quoted, because it is the whole reason this file
exists: *"audit_log had no retention at all and reached 7,026,358 rows / 1.0 GB
before a one-off manual prune cut it back to ~174k"* (`audit-retention.ts:1-11`).

---

## 3. The durable session-event store

Two things share the word "reservation" here, and **conflating them is the
easy mistake**. They are different concepts that happen to have similar shapes.

| | Worker-side durable capacity | Coordinator-side publication claim |
| --- | --- | --- |
| What it protects | a future `closed` event must be *writable* | a committed event must be *published* |
| Owner | `roost-worker`'s store (`apps/worker/src/transport/session-event-store.ts`) | `apps/coord/src/events/pending-event-publications.ts` |
| Bound | 8,192 rows / 8 MiB payloads / 16 MiB database | 256 in-memory entries |
| Dies when | consumed, released, or the store closes | published, claimed, or the process restarts |
| Lives in | the worker's SQLite, across a reconnect | process memory only |

A coordinator that reuses the worker's reservation model would look correct
until the two lifetelycles diverged — which they do, on the first crash.

### 3.1 The write path

One function, `appendEvent` (`apps/coord/src/events/event-transaction.ts`),
is the only durable write. Its order is the contract:

**Before the transaction** (`:76-113`)

1. re-validate an `agent_reference` against the event schema — a failure throws
   `invalid agent conversation reference event` (`:77-83`);
2. cap a worker snapshot at 1,024 sessions (`:84-91`);
3. **normalize** the event: six string fields truncated to 4,096 UTF-8 bytes
   each (`cwd`, `spawn_cwd`, `custom_title`, `git_branch`, `git_remote`,
   `pr_url` — `events/persistence-input.ts:7,12-56`), applied *before* any
   durable write *"so replay and projection stay byte-identical"*. One shared
   normalized value feeds the durable JSON, the projection fold, the
   channel-index publication and the live Sync publication — a second
   normalization is a second answer;
4. **reserve a publication slot** keyed `(worker_fp, client_seq)`
   (`:96-101`). Reserving *before* commit is what makes a concurrent duplicate
   of the same sequence serialize instead of double-publishing.

**Inside one transaction** (`:114-275`)

1. **admission first** (`:116-120`) — see §3.2. A refusal is a *data outcome*,
   not an exception, and it writes **nothing**. This ordering is why a foreign
   worker's auxiliary `extraWork` can never run (`:61-63`);
2. a `workspace_assigned` naming a missing workspace **throws** `workspace is
   unavailable` and rolls back (`:123-131`) — deliberately harsher than
   admission, because the workspace does not exist and retrying cannot help;
3. **snapshot tombstone filter** (`:132-152`): a prior durable `closed` is a
   permanent force-close tombstone, and it is applied to the *effective* event
   **before** it is serialized — *"so the log, projection, route index, and Sync
   publication all see the same set"*;
4. the caller's `extraWork` (`:153`), for writes that must commit atomically
   with the event;
5. **insert into `events`** with the dedupe clause (`:156-175`):
   `ON CONFLICT (worker_fp, client_seq) DO NOTHING`, backed by the partial
   unique index `events_worker_client_seq` (`0004:16-18`). The migration header
   states the intent: *"Coord acks every successful insert OR successful dedup
   so the worker drops the event from its unacked outbox."*
6. **dedupe short-circuit** (`:176-183`): no inserted id plus a non-null
   `client_seq` means already applied — skip the projection, skip publication,
   and still ACK.
7. project: `snapshot` upserts (`:185-190`); `agent_reference` writes only the
   private recovery columns and returns **without** setting `publishable`
   (`:191-205`); `opened` inserts, and **on a lost race deletes its own `events`
   row** so a losing insert leaves no phantom log row (`:208-227`); `closed`
   captures workspace ownership, deletes the session's junction rows, deletes
   the session, and deletes any workspace left with no sessions (`:228-239`) —
   *"the ONLY deletion trigger"*; anything else loads, folds, and updates,
   with `workspace_assigned` also rewriting the junction (`:240-272`).

**After commit** (`:283-323`) and never before.

### 3.2 The post-commit guarantee

**The coordinator commits the event and the public `sessions` projection in one
transaction, then publishes the committed public event**
(`protocol/spec/session-events.md:24`).

The code enforces it structurally, not by convention:

- the transaction body contains no publish call at all;
- the only publisher, `publishCommittedEvent`
  (`events/pending-event-publications.ts:267`), is **module-private**, and its
  only two call sites are inside `resolveEventPublication` — both after the
  awaited transaction has resolved (`:231`, `:258`);
- the module header names it: *"Duplicate delivery and route-before-bus
  ordering are invariants"* (`pending-event-publications.ts:1-3`).

**Route before bus, in one synchronous function** (`:269-274`): apply the durable
channel index, *then* publish to `sessionBus`, *then* the workspace cascade. A
cell frame that routes before the event that named its channel is a frame
nobody can place; the symptom is a terminal that never paints rather than an
ordering error.

### 3.3 Admission — every rule that can refuse an event

`apps/coord/src/events/event-admission.ts`. **Every rejection is a data outcome,
not an exception**: header, *"Worker-originated resource probes fail as a data
outcome, not an exception, so missing and foreign IDs receive neither an ACK nor
a socket-close oracle."* A rejected event gets **no ACK and no close**, so a
prober learns nothing from the difference between "absent" and "foreign".

| # | Check | Line | Result |
| ---: | --- | --- | --- |
| 1 | a non-worker producer short-circuits | `:30-41` | always admitted |
| 2 | the worker row must exist and not be tombstoned | `:42-46` | rejected |
| 3 | `opened`/`snapshot` may not claim another `worker_fp` | `:48-51` | rejected |
| 4 | a dedupe row already exists | `:53-62` | **admitted** — this is what lets a retry reach the claim path |
| 5 | every session in a snapshot belongs to the caller | `:65-67` | rejected |
| 6 | every existing row for an announced id belongs to the caller | `:68-77` | rejected |
| 7 | every workspace named by a snapshot exists | `:79-91` | rejected |
| 8 | an event with no `session_id` | `:96-98` | admitted |
| 9 | session ownership | `:99-107` | rejected if foreign |
| 10 | `agent_reference` for a row that is gone, where the worker has a prior durable `opened` | `:108-120` | **admitted** — *"A reference queued before an offline force-close must still be consumed or it permanently blocks the worker's ordered durable replay."* |
| 11 | an unknown session for any kind but `opened` | `:121` | rejected |
| 12 | `opened` for a genuinely new session | `:122` | admitted |

Rule 10 is the one that looks like a hole and is not: a reference for a session
the coordinator force-closed offline would otherwise wedge the worker's ordered
durable replay forever.

### 3.4 The transport gates in front of admission

`apps/coord/src/workers/worker-frame-dispatch.ts`, before `appendEvent` is even
called:

- a decode failure or a null decode drops the frame with no ACK and no close
  (`:84-116`);
- **the snapshot-readiness gate** (`:107-121`): before the snapshot barrier, only
  `opened`, `closed`, `respawned`, `agent_reference` and `snapshot` are admitted.
  Anything else is dropped **silently, with no ACK**, so the worker replays it
  after its snapshot;
- **the exclusive write gate** (`:122-127`): *"A held keeper-update fence
  withholds the ACK so CoordLink replays the preserved entry after the update.
  Acking here loses the durable record of which PTYs are live."*;
- the **post-commit generation fence** (`:170,191`): a superseded connection
  generation is neither published nor ACKed — *"A dedupe/stale generation did
  not install this connection's exact live set. It cannot cross readiness and
  receives no ACK."*;
- a **replay whose payload differs** from the retained unpublished effect is a
  **protocol violation**: `signal("worker.protocol_violation", { reason:
  "event_dedupe_payload_mismatch" })` plus a socket close (`:174-186`).

### 3.5 The worker's reservation rules — do not re-derive them

`roost-worker`'s `event_store.rs` already ports this. The rule, restated so the
coordinator side knows why it never sees a half-written session:

> A session that is open has not yet written its `closed` event, and it must be
> able to. If the store fills up in the meantime, that close is unwritable — and
> a session that cannot record that it ended is a session a client believes is
> still running. So opening a session **reserves the capacity its close will
> need**, before anyone knows whether the close will happen.

The reservation is a claim with a lifetime, not a queue entry. `hold` is what
makes it work: the session is committed, so its close capacity is no longer
speculative, and the same token remains the sole owner of that close — but it
stops blocking snapshots. In v2 that is `holdSessionEvent`
(`apps/worker/src/transport/session-event-store.ts:142-154`), which clears one
boolean and decrements one counter and **touches nothing else**: the token stays
in the map and the rows and bytes stay counted. Only the `snapshotBlocking`
property is released. A reservation blocks a reconnect snapshot
(`apps/worker/src/transport/coord-link-unacked.ts:126-128`) so the snapshot can
never omit a session whose `opened` is still in flight.

Bounds: 8,192 rows, 8 MiB of payloads, a 16 MiB database, sequences allocated
in blocks of 1,024 (`session-event-store-limits.ts:5-8`). A block is claimed
and written as a unit, so a crash costs the unused tail of one block rather than
renumbering every event after it — *"a GAP in the sequence, never a repeat — a
repeat would let a replayed event be mistaken for a new one."*

**The coordinator's side of that same guarantee** is the publication claim in
§3.6, and it is a different mechanism entirely.

### 3.6 The publication claim — the coordinator's own bounded thing

`apps/coord/src/events/pending-event-publications.ts`. Header: *"Owns bounded
same-process recovery for committed worker events whose live publication lost a
connection-generation race."* Per slot: `reserved | retained | claimed`.

The race: the durable row committed, then `canPublish()` — the
socket-generation and revocation fence — returned false, or the publish itself
threw. The event is durable and nobody was told.

- `PENDING_EVENT_PUBLICATION_MAX_ENTRIES = 256` (`:11`); exceeding it throws
  *"pending event publication capacity exceeded"* (`:92-94`) **before** the
  transaction opens.
- `claim` compares the retained `eventJson` **byte for byte** (`:136-146`); a
  mismatch is the `replayRejected` protocol violation of §3.4.
- `clearWorker(fp)` drops every slot for a fingerprint on key revocation
  (`main.ts:103-106`) and on worker delete (`handlers-workers.ts:242-243`).

**On a crash with entries pending:** the store is **process memory only**
(`:60-62`), so a crash discards every retained effect. That loses the *live
push*, not the *fact*: the worker still has the entry unacked, replays the same
`client_seq`, admission rule 4 admits it, the INSERT dedupes, and the worker
gets its ACK. A browser recovers the event from the durable log instead — the
Sync feed's `getEventMaxId` cutoff and paged `getEventsThrough`
(`apps/coord/src/sync/sync-feed.ts:304-360`). The 256-entry bound and the whole
map are rebuilt empty on restart; the only cross-process state is the SQLite
unique index.

### 3.7 Visibility — one predicate, and it is coordinator-local for now

`apps/coord/src/events/session-event-visibility.ts`:

```ts
export const PRIVATE_SESSION_EVENT_KIND = "agent_reference" as const;
export function isPublicSessionEvent(event: SessionEvent): event is PublicSessionEvent {
  return event.kind !== PRIVATE_SESSION_EVENT_KIND;
}
```

Header: *"Durable queries, live publication, and frame construction all depend on
this predicate so browser lanes cannot diverge."* Five consumers: the three
durable readers filter `kind != "agent_reference"` (`events/event-query.ts:19,37,55`),
the publisher returns early on a private event
(`pending-event-publications.ts:268`), and the row mapper omits the three
agent columns entirely (`events/event-projection.ts:22-24`).

So `agent_reference` is **durable but private**: the owning worker recovers it,
no browser sees it through either the log reads or the bus. Every other variant
is public on both lanes.

> **Intended home, not yet moved.** This predicate belongs in `roost-protocol`
> beside `foldEvent`, because the durable readers, the bus publisher and the
> Sync frame constructor are three consumers in three crates and a coordinator
> -local copy is one implementation too many. Until the move lands,
> `roost-coord` defines it once, locally, and this document is the pointer.

### 3.8 The reads

Three durable reads over the single global `events` stream, all excluding the
private kind (`apps/coord/src/events/event-query.ts`):

| Read | Window | Limit | Used for |
| --- | --- | ---: | --- |
| `getEventsSince(sinceId)` | `id > sinceId` ascending | 1,000 | reconnect backfill |
| `getEventMaxId()` | `max(id)` over public rows | — | the recovery cutoff, captured *after* live subscription |
| `getEventsThrough(cursor, cutoff)` | `cursor < id <= cutoff` | 256 | one stable recovery interval |

There is **no** retention on the `events` table. That is a deliberate gap in v2,
not an oversight this port should quietly close.

---

## 4. Auth: the credential, the principal, and the boot invariant

### 4.1 There is no worker API key

The premise this section was written against is wrong, and it is worth stating
plainly because it is the kind of wrong that becomes a second implementation:
**there is no bearer API key, no shared secret, no HMAC, and no `x-api-key`**
anywhere in `apps/coord/src`.

There is exactly one cryptographic credential shape, used by both principals:

- a long-lived **Ed25519 device key** — OpenSSH PEM on a worker
  (`apps/worker/src/host/jwt.ts:47-92`, mode `0600`), non-extractable WebCrypto
  in a browser (`apps/web/src/client/auth/web-key.ts:98`);
- whose **fingerprint** — *lowercase hex SHA-256 of the raw 32-byte public key*
  (`packages/protocol/src/fingerprint.ts:18-27`) — is both the JWT's `kid` and
  its `sub`;
- signed into a short-lived **EdDSA JWT**.

The second credential artifact is not a key at all: the **bootstrap token** is a
one-shot enrollment grant (`roost_bt_` + 48 lowercase hex, 24 h TTL) that lets a
*new* key be redeemed into a principal. It is stored as a SHA-256 digest only.

### 4.2 Where the credential is presented — and where it never is

Three surfaces, and **credentials never enter a query string**:

| Surface | Presentation |
| --- | --- |
| Connect | `Authorization: Bearer <jwt>` (`apps/coord/src/auth/auth-interceptor.ts:174-176`) |
| worker WS `/ws/coord-worker/<fp>` | the **second** `sec-websocket-protocol` entry; marker `roost-worker-auth` (`worker-ws-upgrade.ts:53-62`) |
| Sync WS `/ws/coord-sync` | the second subprotocol entry; marker `roost-auth` (`sync-ws-upgrade.ts:137-142`) |

The worker endpoint bans the **entire** query surface, not just a known
parameter: *"This endpoint has no query contract. Rejecting the entire query
surface guarantees an old `?token=` client cannot leak a credential into access
logs while still authenticating successfully by subprotocol."*
(`worker-ws-upgrade.ts:42-44`)

The Sync endpoint does accept a query — `tab`, `since`, `flow`, `sync_v` — so
"credentials never enter the query" is a **per-transport** rule, not a global
one. See §9.

The `Authorization` prefix match is **case-sensitive** `"Bearer "`
(`auth-interceptor.ts:175`); anything else, `bearer` included, is simply
unauthenticated.

### 4.3 The JWT, byte-exactly

Header, identical from all three minters (`apps/coord/src/auth/jwt.ts:244`,
`apps/worker/src/host/jwt.ts:242-243`, `apps/web/src/client/auth/web-key.ts:111`):

```json
{"alg":"EdDSA","typ":"JWT","kid":"<64 lowercase hex>"}
```

Payload: exactly four claims, no `iss`, no `nbf`, no `jti`.
`{sub, aud, iat, exp}`, with `aud` the **string** `"roost-coordinator"`.

```
signingInput = b64url(JSON(header)) + "." + b64url(JSON(payload))
signature    = Ed25519 over UTF-8(signingInput)     // 64 raw bytes
token        = signingInput + "." + b64url(signature)  // unpadded base64url
```

Non-standard items a port **must** reproduce:

1. **unpadded** base64url. Four separate implementations exist across the three
   ends; only unpadded interoperates — the browser hand-strips `=`
   (`web-key.ts:274-281`) where the others use `Buffer`. In v3 this is
   `roost_host::jwt_base`, whose header says why it is strict: *"a segment that
   is not canonical base64url is refused rather than silently resolved to bytes,
   so one token cannot be spelled two ways and still verify."*
2. **`kid` is a bare 64-hex fingerprint**, not a JWK thumbprint (`jwt.ts:3`).
3. **the SPKI DER prefix is hand-rolled** — the 12 bytes
   `30 2a 30 05 06 03 2b 65 70 03 21 00` followed by the raw 32-byte key
   (`jwt.ts:15-17`). Keep it byte-for-byte.
4. **`sub` must equal `kid`** (`jwt.ts:205`), and that is the *entire* issuer
   check: *"The subject is the authorized key selected by `kid`."* A valid
   signature cannot claim another principal.
5. **`aud` may be a string or an array**; a missing `aud` coerces to
   `[undefined]` and fails as `wrong aud: undefined` (`jwt.ts:194-197`).
6. **`typ` is emitted but never validated** — only `alg` is (`jwt.ts:159`).

Time bounds, all `401`:

| Check | Line | Message |
| --- | --- | --- |
| `exp * 1000 <= now` | `:212-214` | `token expired` |
| `iat + jwtMaxAgeSecs < now` | `:215-217` | `token too old` |
| `iat > now + 30` | `:218-220` | `token from the future` |

**Clock skew is +30 s forward and 0 s backward** — asymmetric, and hard-coded
(`jwt.ts:218`).

**The server-side ceiling is real and easy to miss:**
`validUntilMs = min(exp, iat + jwtMaxAgeSecs)` (`jwt.ts:225-228`). *"Both
timestamps are bounded by this verifier: `exp` is the credential's explicit
deadline and max-age remains a server-side ceiling even when a signer asks for
longer."* `jwtMaxAgeSecs` defaults to **300** and is
`ROOST_COORDINATOR_JWT_MAX_AGE_SECS`; the browser's own token life is 300 s
with a 240 s cache (`web-key.ts:23-24`). A port that honours only `exp` accepts
credentials the v2 coordinator would have refused.

**Generation fencing.** `kid` lookups are re-checked against
`cache.generations[kid]` at four points — before and after key import, after
signature verification, and after claim validation (`jwt.ts:108,117,181,222`). A
generation bump mid-verify is a 401. This is a revocation race fence, not
redundancy. And the two invalidation calls differ only in whether they bump the
generation: `invalidateJwtKey` bumps and drops (revoke/rotate/logout),
`refreshJwtKey` drops only (after mint/redeem/pair, so a verifier that already
loaded a row stays valid) — `jwt.ts:79-89`. Conflating them breaks either
enrollment or revocation.

### 4.4 The principals

`apps/coord/src/auth/auth-principal.ts:6-33` — three kinds, and note the second
`Caller` type: `jwt.ts:126-132` defines a **key** identity
(`{fingerprint, label, scopes?, keyGeneration, validUntilMs}`), while
`auth-principal.ts` defines the **principal** union. A port needs both, and the
dispatcher stores the principal.

```
account-device   { kind, fingerprint, label, accountId }
worker           { kind, fingerprint, label }
legacy-self-hosted{ kind, fingerprint, label }
```

`resolveCallerPrincipal` left-joins `authorized_keys → account_devices →
accounts` and `→ workers`, and refuses:

| Condition | Line | Result |
| --- | --- | --- |
| no `authorized_keys` row | `:60` | `null` |
| **both** a device and a worker | `:66` | `null` — *"A key must never acquire two kinds of authority."* |
| device present, `account_id` null or the account is not `active` | `:69` | `null` |
| worker present, `workers.deleted_at_ms` is not null | `:78` | `null` |
| neither | `:85-89` | `legacy-self-hosted` |

`legacy-self-hosted` is a **third, browser-capable** kind. It is *not* rejected
by `requireAccountDevice`, but it **is** 404'd at the Sync WebSocket
(`sync-ws-upgrade.ts:162-170`) because it carries no runtime identity.

### 4.5 Who may call what

The interceptor is **not** an authorization gate. It reads the credential,
stamps context, takes a write-gate lease, and writes the audit row. The actual
authorization is `requireAccountDevice` / `requireWorker` /
`requireSearchTabId` / `optionalAccountDevice` / `assertOnHost`, called
**inside handlers** (`auth-interceptor.ts:256-302`).

Six methods are unauthenticated, and five of the six are gated by a secret in
the request body instead: `AuthCoordIdentity` (public), `AuthRedeemWorker`,
`AuthRedeemBrowser` (bootstrap token), `PairCreate`, `PairPoll`, `PairConfirm`
(requester token, plus a verification code for confirm). Everything else calls
a `require*`.

`x-roost-auth-layer: device` accompanies every *"authentication required"*
(`auth-interceptor.ts:256-262`).

The three `x-roost-*` trust headers — `x-roost-remote-addr`,
`x-roost-listener-trust`, `x-roost-on-host` — are **stripped from the inbound
request and re-stamped by the listener** (`rpc/bun-handler.ts:53-63`): *"The
strip-then-set guards below also prevent browser-supplied origin values from
reaching trust-gated handlers."*

### 4.6 The write gate and `WRITE_METHODS`

40 method names hold the exclusive write gate (`auth-interceptor.ts:111-132`),
including all six auth/pairing mutations. The comment on the list is the reason
terminal writes are **not** in it: *"Terminal writes acquire their lease only
after entering the per-sender/session FIFO. Taking one here would let queued
input or a prompt hold the exclusive keeper-update drain open."*
(`:114-116`)

`acquire()` throws `Unavailable: coordinator keeper update preparation in
progress` while an exclusive drain is live
(`apps/coord/src/coordinator-write-gate.ts:31-38`).

### 4.7 Authorized keys

`ROOST_COORDINATOR_AUTHORIZED_KEYS`, default
`<coord data dir>/authorized_keys.roost`; the file is **OpenSSH
`authorized_keys` format**, one entry per line, blank and `#` lines ignored.
Field 0 must be exactly `ssh-ed25519`; field 1 base64-decodes to at least
`4+11+4+32 = 51` bytes and the key is `raw[19..51]`
(`apps/coord/src/auth/authorized-keys.ts:16-31`).

- A **malformed entry is silently skipped** (`:78`) — no log, no error, not
  counted. The whole import is wrapped in a try/catch at boot and downgraded to
  `log.warn("main", "authorized_keys_import_failed")` (`main.ts:73-80`).
- **Revoked keys are never re-imported** (`:81`), and a trigger enforces the
  same at the database.
- Import upserts on `fingerprint` and updates `label` only; `added_at` is
  preserved (`:84-89`).
- If exactly one `active` account exists, every imported key that is not a
  `workers.fp` also gets an `account_devices` row (`:91-101`). Zero or two or
  more accounts means no auto-association.
- **There is no development default key.** No code path generates a keypair or
  skips auth; `roost quickstart` mints a host-local bootstrap token instead.

### 4.8 The boot invariant

`apps/coord/src/auth/self-hosted-tenant.ts`. One deployment shape means exactly
one account, one organization, one dashboard. `SelfHostedTenant` is
`{accountId, organizationId, dashboardId}` (`:7-10`) and every scoped write
takes its values from it.

All of it inside one `BEGIN IMMEDIATE … COMMIT` (`:355-372`). Topology checks
(`:81-207`): at most one of each; if zero accounts, everything else must be
empty or *"identity topology is partial"*; if one, exactly one of each; all
three `active`; the dashboard belongs to the organization; the org membership
is exactly `(orgId, accountId, 'owner')`; the dashboard membership is exactly
`(dashId, accountId, 'admin')`.

Key checks (`:209-261`): no fingerprint that is both a worker and an account
device; no device row for another account; no device row without a key. Then
**auto-associate every non-worker key to the sole account** and re-validate.

Scope checks (`:263-343`) over `workers`, `bootstrap_tokens`, `events`,
`sessions`, `workspaces`, `workspace_sessions`, `tasks`, `mcp_relays`,
`push_subscriptions`, plus `audit_log` and `app_settings`: no foreign
`dashboard_id`. `app_settings` has one deliberate exception — the
coordinator-global `push.vapid` row is stored with a **NULL** scope
(`0029_global_push_vapid_identity.sql`).

**What breaks if it does not hold:** `fail()` throws
`SelfHostedTenantInvariantError` with the message
`self-hosted tenant invariant violation: <reason>` (`:35-39`). The throw
escapes `main.ts` **before the listener is constructed**
(`main.ts:83` runs before `createCoord` at `:100` and
`startBunCoordinatorListeners` at `:208`), so **the coordinator never binds its
port**. That is the intended failure: a mis-scoped database is not repaired by
serving traffic against it.

The message prefix is load-bearing — tests assert it and assert the whole
message is under 160 characters (`apps/coord/tests/self-hosted-tenant.test.ts:152-156`).

Enforced at three points, all ordered **after** the authorized-keys import so
rule 11 sees fresh keys: the `0024` migration hook
(`main.ts:68, backfillLegacyScopes: true`), immediately after
(`main.ts:83, false`), and a host-local token mint
(`bootstrap-tokens.ts:110`).

### 4.9 Pairing

Seven RPCs, not six: `PairCreate`, `PairPoll`, `PairList`, `PairApprove`,
`PairConfirm`, `PairDeny`, `PairApprovalStatus`. Approval alone grants **no
authority**; only `PairConfirm` writes the key
(`protocol/spec/auth-and-pairing.md:9`).

```
(insert)                 ──► pending
pending            ──Approve──► verification_required
pending|verification_required ──Deny──► denied
pending|verification_required ──expiry──► expired
verification_required ──Confirm, good code──► completed
verification_required ──5 bad attempts──► verification_failed
```

| Bound | Value | Source |
| --- | ---: | --- |
| `PAIR_REQUEST_TTL_MS` | 600,000 (10 min) | `handlers-pairing.ts:52` |
| `MAX_PENDING_PAIR_REQUESTS` | 32 | `pairing-account.ts:16` |
| `PAIR_VERIFICATION_ATTEMPT_LIMIT` | 5 | `pairing-secrets.ts:23` |
| `PAIRING_CEREMONY_VERSION` | 1 | `packages/protocol/src/pairing.ts:5` |
| verification code | 6 ASCII digits, unbiased | `pairing.ts:11-14,26-33` |
| request id | 16 bytes / 32 lowercase hex | `pairing.ts:35-37` |
| requester token | 32 bytes / 64 lowercase hex | `pairing.ts:39-41` |
| bootstrap token | `roost_bt_` + 48 hex, 86,400,000 ms TTL | `bootstrap-tokens.ts:45-52` |

Every secret is **client-generated**; the coordinator only ever stores a
SHA-256 hex digest. `verification_code_hash` is cleared on **every** terminal
transition. The 32-request ceiling exists because *"Pair polling receives its
own 600-request/minute route budget so every live requester can share one
NAT"* (`middleware/rate-limit.ts:1-3`) — one NAT, many requesters, so the
ceiling is about abuse, not capacity.

Provenance (`user_agent`, `client_browser`, `client_os`, `client_device_type`,
`source_ip`, `country_code`, `region`, `city`, and the three
`edge_identity*` columns) is **server-observed, never taken from the request
body**: *"Capture server-observed pairing metadata without trusting request-body
claims."* (`auth/pair-request-provenance.ts:57`) Geo headers are read **only**
under a trusted proxy, and `countryCode` must match `^[A-Z]{2}$`
(`:69-78`). On completion these are copied onto `authorized_keys` as
`paired_from_ip`, `paired_country`, `paired_user_agent`, `paired_edge_identity`.

### 4.10 The socket authentication deadline

`apps/coord/src/auth/ws-auth-deadline.ts` arms a timer for
`min(deadline - now, 2_147_483_647)` and **re-arms** on each fire, which is what
makes an arbitrarily distant deadline safe on a 32-bit timer
(`ws-auth-deadline.ts:36-49`). On expiry: `ws.close(4003, "reauth required")`
(`:42`).

**In v2 production this never fires.** `reauthAtMs` defaults to `null`
(`sync-ws-upgrade.ts:122`) and the only call site omits it
(`bun-coordinator-listeners.ts:328`). `verifyJwt` already computes the exact
value needed — `validUntilMs` (`jwt.ts:225-228`) — and logs it without wiring
it (`worker-conn.ts:376-377`). v3 should close that gap: the deadline is
available, the close code is already defined, and a socket that never re-auths
is a socket whose credential outlived its ceiling. See §9.

### 4.11 The rate limiter

`apps/coord/src/middleware/rate-limit.ts`.

- Bucket key: `route path` + `NUL` + client address. **The route list is by
  exact RPC name, 44 entries** (`:16-64`) — not a prefix, because *"Prior shape
  used prefix `/roost.v1.CoordinatorService/Workspaces` which matched
  WorkspacesList (called on every SPA bootstrap + visibilitychange focus
  refresh), eating the same 100/min bucket as create/update/delete
  mutations."* (`:7-14`)
- Window 60,000 ms; 100 tokens per window for every listed route; **600 for
  `PairPoll` only** (`:66-67`).
- `RATE_LIMIT_MAX_BUCKETS = 10_000`, with LRU maintenance by insertion order:
  *"A full map of live buckets fails closed rather than evicting an active limit
  and giving a churning caller a fresh budget."* (`:99-104`)
- `GET`, `HEAD` and `OPTIONS` are exempt (`:237-239`).
- Over the limit: `429`, `{"error":"rate limit exceeded"}`,
  `retry-after: max(1, ceil(remainingMs/1000))` (`:186-196`).

### 4.12 Caller origin, and the loopback admission gate

`caller-origin.ts` has two trust profiles, and the difference is the security
property:

- `direct`: `clientIp` is the socket peer; `onHost` is a loopback peer.
- `trusted-proxy`: `clientIp` is the first `X-Forwarded-For` entry;
  **`onHost` requires the header's *absence*** (`:37-50`) — *"The operator's
  front door overwrites X-Forwarded-For with the address it authenticated. Its
  presence therefore proves the request traversed a proxy, which is what
  disqualifies it from on-host authority."*

`assertOnHost` throws `PermissionDenied: on-host only` (`:53-57`).

`coordinator-request-admission.ts` runs **first**, before any upgrade, and only
on a loopback bind. It rejects DNS-rebinding requests on `Host` and `Origin`,
`403` with `forbidden host` or `forbidden origin` (`:76-79`). The host
comparison parses under both `http://` and `https://` and keeps only bare
authorities with no userinfo, path, query or fragment (`:51-73`) — a prefix or
regex on `127.0.0.1` *"would admit an attacker's page served from another
loopback port."* Until the listener's port is known it **fails closed** with
`503 listener unavailable` (`bun-coordinator-listeners.ts:363-365`).

### 4.13 Security headers

`security.ts` plus `packages/host/src/http-security.ts`. On an allowlisted
origin: `access-control-allow-origin` plus
**`access-control-expose-headers: x-roost-auth-layer`** (`:63`) — without that
exposure a browser cannot read the header that tells it *which* auth layer
refused it. Unconditionally: `vary: origin, access-control-request-method,
access-control-request-headers`, `access-control-allow-methods: *`,
`access-control-allow-headers: *`. Plus CSP, `X-Frame-Options: DENY`,
`X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`,
`Permissions-Policy: camera=(), geolocation=(), microphone=(self)`, and HSTS
when the operator trusts a proxy. `wrapResponse` mutates in place — *"Mutate in
place: re-wrapping converts a `Bun.file()` body into a `ReadableStream`, which
drops content-length"* (`security.ts:73-75`).

One audit exclusion, with its incident: a Connect `401` with **no caller** from
a **trusted proxy** is not persisted, because *"an internet-facing scanner would
otherwise grow audit_log without bound (it once reached 7,026,358 rows / 1.0
GB)"* (`security.ts:126-134`).

---

## 5. The RPC router

### 5.1 One service, and why

`CoordinatorService` is the browser-to-coordinator surface at
`POST /roost.v1.CoordinatorService/<Method>`; binary Connect is authoritative
(`protocol/spec/coordinator-rpc.md:9`).

**The hazard.** Each domain factory returns a `Pick<ServiceImpl<…>>` spread into
one `router.service()` object literal
(`apps/coord/src/rpc/router.ts:113-137`). A second `router.service()` call
**shadows the rest with unimplemented throws**, because Connect stubs every
absent method. The rule is repeated in the file header (`:1-5`), inside the
literal (`:114-118`), in `apps/coord/README.md:71` — *"A second service call
shadows the first with unimplemented methods; never add one."* — and in
`protocol/spec/coordinator-rpc.md:29`.

**There is no lint rule for it.** The only mechanical guard is test-shaped: both
relevant v2 tests build the real router and assert each `requestPath` appears
**exactly once** in `router.handlers`
(`apps/coord/tests/agents/agent-status-handlers.test.ts:169-185`,
`apps/coord/tests/attachments/attachment-direct-handlers.test.ts:198-210`).

**In Rust the hazard is inverted, and that is strictly better.** A generated
trait requires every method, so a missing delegation is a **compile error**, not
a runtime 501. The guard the v2 tests provided becomes a table of method routes
whose coverage is asserted against the proto's own service block
(§5.3). What Rust cannot do is express "one service literal", so the single
`impl CoordinatorService` is split across sibling files by domain — legal
because Rust permits several `impl Trait for Type` blocks, and it changes
nothing: **one type, one implementation, 100 % of the trait or it does not
compile.**

### 5.2 The domain spread index

18 factories, in the order they are spread
(`apps/coord/src/rpc/router.ts:119-136`):

| # | Factory | Methods | Auth |
| ---: | --- | ---: | --- |
| 1 | `makeWorkerHandlers` | 7 | `WorkersRegister`/`Heartbeat`: worker; list/rename/delete/deploy: device |
| 2 | `makeWorkerUpdateHandlers` | 1 | device + update/on-host policy |
| 3 | `makeSessionHandlers` | 15 | `SessionsList`: device, or worker restricted to its own open recovery; all others: device |
| 4 | `makeAgentStatusHandlers` | 3 | device |
| 5 | `makeAgentPromptHandlers` | 1 | device + authorized open session/status fence |
| 6 | `makeWorkspaceHandlers` | 5 | device |
| 7 | `makeTaskHandlers` | 5 | device |
| 8 | `makeMcpHandlers` | 4 | device |
| 9 | `makeAuthHandlers` | 15 | mixed: public identity/bootstrap redemption, device mint/logout/devices, pairing per §4.9 |
| 10 | `makeSystemHandlers` | 6 | health public; DB-export URL device **+ on-host**; rest device |
| 11 | `makeTranscriptionHandlers` | 4 | device |
| 12 | `makeAgentConfigHandlers` | 2 | device |
| 13 | `makeAttachmentHandlers` | 8 | device |
| 14 | `makeAttachmentDirectHandlers` | 2 | device + exact tab/grant descriptor |
| 15 | `makeAttachmentPeerHandlers` | 1 | device + exact tab/grant/worker fence |
| 16 | `makeUiHandlers` | 4 | device + persisted session/current socket generation |
| 17 | `makePushHandlers` | 3 | device |
| 18 | `makeStreamingHandlers` | 1 | device, then `Unimplemented` |

**87 of the 103 proto-declared methods are wired.** The 16 that are not answer
Connect `Unimplemented` / HTTP 501: twelve auth methods
(`AuthDashboardAccess`, `AuthOwnerActivate`, `AuthPasswordResetRequest`,
`AuthPasswordResetRedeem`, `AuthPasswordLogin`, `AuthFederatedContinue`,
`AuthCredentialsGet`, `AuthPasswordAdd`, `AuthFederatedLinkBegin`,
`AuthFederatedLink`, `AuthMintCoordinatorRelocation`,
`AuthRedeemCoordinatorRelocation`), the three `CoordinatorMove*` methods, and
`MiscFlags`. Every one of them is **absent from `apps/coord/src` entirely** —
verified by grep, not inferred from the router.

The proto also declares managed-account, dashboard and relocation surface that
`makeAuthHandlers` does not implement. Proto presence is not implementation;
`protocol/spec/coordinator-rpc.md:94` says so, and it is right.

### 5.3 The route table is the contract

Every method needs three facts before it can be called: its owning domain, its
authorization requirement, and whether it is wired. Those are data, so they live
in one table with a test asserting its coverage against
`coordinator.proto`'s service block. A method added to the proto without a row
fails the test; a row for a method not in the proto fails the test.

### 5.4 Error codes

Standard Connect codes, with the audit-status mapping
(`apps/coord/src/auth/auth-interceptor.ts:58-78`, audit-only — Connect's own
HTTP mapping is separate):

| Code | HTTP | Meaning |
| --- | ---: | --- |
| `InvalidArgument`, `OutOfRange` | 400 | malformed request or failed field validation |
| `Unauthenticated` | 401 | missing/invalid authority where the method requires it |
| `PermissionDenied` | 403 | wrong principal kind, resource scope, tab, or on-host requirement |
| `NotFound` | 404 | missing/foreign resource, **normalized where required** |
| `AlreadyExists`, `Aborted` | 409 | identity collision or concurrent transition |
| `FailedPrecondition` | 412 | invalid lifecycle/version/ordering state |
| `ResourceExhausted` | 429 | bounded owner/capacity rejection |
| `Unimplemented` | 501 | retired or moved surface |
| `Unavailable` | 503 | required worker/socket offline or not routable |
| `DeadlineExceeded` | 504 | bounded downstream wait expired |
| unhandled | 500 | |

Two normalizations matter and are easy to lose:

- `pairing` deliberately answers **generic `NotFound`** for an unknown *or*
  foreign request/token (`handlers-pairing.ts:161,179,334`) so polling cannot
  distinguish "never existed" from "not yours";
- `requireSessionWorkerSocket` has exactly two strings,
  `NotFound "session not found"` and `Unavailable "worker offline"`
  (`apps/coord/src/rpc/router-helpers.ts:29-41`), and a browser distinguishes
  them.

### 5.5 Audit policy

`AUDIT_SKIP_METHODS` — 14 methods whose **successes** are not written
(`auth-interceptor.ts:135-144`). `AUDIT_NEVER_PERSIST_METHODS = { PairPoll: true }`
(`:147-149`): *"PairPoll is token-bound anonymous high-volume polling and never
persists audit rows."* `PairConfirm` **failures do** persist (`:152-162`). A
successful `PairConfirm` writes an explicit success row.

### 5.6 Pending RPCs — why the coordinator must keep a correlation table

Header (`apps/coord/src/router/pending-rpcs.ts:1-11`): *"Coord-side correlation
table for browser→worker RPCs that need a reply (sessions.spawn returns
session_id+channel_id, sessions.attach returns replay_offset, etc.). Mutation
creates a pending entry, sends the browser-command downstream to the worker,
awaits the promise; worker upstream `rpc-ok`/`rpc-error` resolves it."*

Without it, the worker's answer arrives on a different transport, unordered and
unsolicited, with no request context — so the awaiting handler could never
resolve, and a buggy or hostile worker could settle **another worker's**
request. The key is `JSON.stringify([workerFp, requestId])` (`:36-38`):
*"Map keys include the authenticated worker fingerprint so client-supplied upload
ids cannot replace another worker's pending completion."* An unscoped lookup is
honoured only when the id is unique across all entries (`:60-67`).

Settles exactly once, through `takePending` (`:69-75`), which clears the timer
and deletes the key: resolve, permanent `Internal` reject, retryable
`Unavailable` reject, browser `Canceled` cancel, 30 s `DeadlineExceeded`, or
bulk rejection when the worker's socket dies. A duplicate key throws
`AlreadyExists` (`:90-94`). Timers are unref'd.

### 5.7 Implementing a generated Connect service in Rust — the four details that matter

This is a **reusable fact for every crate in this workspace that implements a
generated `connectrpc` service**, not a coordinator note. It cost three wrong
attempts to find, and each wrong attempt fails with an error that points at the
wrong thing.

The trait `connectrpc-build` generates for a unary method is

```rust
fn method<'a>(
    &'a self,
    ctx: RequestContext,
    request: ServiceRequest<'_, Req>,
) -> impl Future<Output = ServiceResult<impl Encodable<Resp> + Send + use<'a, Self>>> + Send;
```

and exactly one impl form satisfies it:

```rust
fn method<'a>(&'a self, _ctx: RequestContext, _r: ServiceRequest<'_, Req>)
    -> impl Future<Output = ServiceResult<impl Encodable<Resp> + Send + use<'a>>> + Send
{
    handler()
}
```

Four details are load-bearing, and each of the three obvious alternatives is a
real error:

1. **`async fn` CANNOT satisfy it.** An `async fn` desugars to an opaque with no
   `+ Send` on the future, so the signature does not match
   (`impl item signature doesn't match trait item signature`). Write the
   `-> impl Future<...> + Send` form and return a future from the body. The one
   place an `async` block is still needed is a method that computes its reply
   rather than delegating — there, `async move { … }` produces the same future
   with the bound present.
2. **`use<'a, Self>` is E0799 in an impl.** A precise-capture list may name only
   *generic parameters*, and in `impl Trait for ConcreteType` `Self` is an alias.
   The impl therefore writes `use<'a>` and nothing more, which is a **subset** of
   the trait's `use<'a, Self>` and is accepted.
3. **The request lifetime is `ServiceRequest<'_, Req>` and must stay fresh.**
   Writing `ServiceRequest<'a, Req>` ties it to `'a` and introduces a second
   lifetime the opaque may not capture. The generated `use<'a, Self>` exists
   precisely to forbid the response borrowing the request, and v2's
   `bun-handler.ts:93-110` relies on that same separation when it buffers a unary
   body so the response cannot outlive it.
4. **The two server-streaming methods need `use<>`, not `use<'a>`.** Their
   generated signature captures `use<Self>` only and declares no `'a`, so
   `use<'a>` refers to a lifetime that is not in scope.

**A fifth, about the layout rather than the signature: Rust does not allow one
trait's impl to be split across blocks, even when the method names are fully
disjoint.** Two `impl CoordinatorService for CoordinatorServiceImpl` blocks are
`E0119 conflicting implementations`. This was verified with a twenty-line
program rather than assumed, because it is the opposite of the intuition (inherent
impls *may* be split, and `impl Trait for &T` and `impl Trait for T` are
different impls). It is why all 103 methods live in one
`crates/roost-coord/src/rpc/service_impl.rs` with a recorded size exception,
rather than in the twelve per-domain files the v2 factory split suggests.

**And the cascade worth knowing about:** a missing `use` in any of those files
makes the impl target an *error type*, and then `Self` in `use<'a, Self>` reports
as "an alias", every method reports as not-a-future, and every method reports as
a signature mismatch. One missing import produced 248 errors across three
classes. Read the first `E0425` in the file, not the hundred that follow it.

The generated output types are **not** uniformly `<Method>Response`. Seven
methods name something else — `AgentConfigGet`/`AgentConfigSet` return
`AgentConfig`, the four `Transcription*` config methods return
`TranscriptionConfig`, `AuthPasswordResetRequest` returns
`AuthPasswordResetStartResponse`, and the two streaming methods return
`*Frame`/`FirehoseFrame`. Read them out of
`protocol/proto/roost/v1/coordinator.proto` rather than deriving them, which is
what `tests/method_route_coverage.rs` does.

---

## 6. The HTTP listener

One listener multiplexes five surfaces, and the **order** is load-bearing
(`apps/coord/src/bun-coordinator-listeners.ts:314-352`).

| # | Surface | Path | Auth | Unauthenticated result |
| ---: | --- | --- | --- | --- |
| 0 | Host/Origin admission | any | none | `403` `forbidden host` / `forbidden origin`; `503 listener unavailable` before the port is known |
| 1 | worker upgrade | `/ws/coord-worker/<64-hex>` | `roost-worker-auth,<jwt>` | §7 |
| 2 | Sync upgrade | `/ws/coord-sync` | `roost-auth,<jwt>` | §8 |
| 3 | retired Connect `Sync` | `POST /roost.v1.CoordinatorService/Sync` | none | **`410 Gone`**, body `sync moved to /ws/coord-sync` |
| 4 | Connect RPC | `POST /roost.v1.CoordinatorService/<Method>` | per method | §5.4 |
| 5 | DB export | `GET/HEAD /api/db-export` | **on-host only** | `403` `{"error":"on-host only"}` |
| 6-7 | namespace misses | `/roost.*`, `/api/*` | none | `404 not found` |
| 8 | SPA / static | everything else, GET/HEAD | Cloudflare Access when configured and not on-host | `404` when the assertion is missing or invalid |
| 9 | CORS preflight | `OPTIONS` any | none | `204` + CORS + security headers |

**Step 3 comes before Connect opens the stream, deliberately.** The handler stub
alone is not enough: a throwing stub still lets Connect open a response stream,
keeping the runtime's abort-listener crash path reachable. A plain unary
`Response` is never abort-tracked (`bun-coordinator-listeners.ts:338-340`).
Callers therefore get **410**, not the handler's 501; and because the handler
authenticates *first*, an unauthenticated caller reaching the stub would get
401 — which is exactly why the guard is at the fetch layer.

**Transport policy.** Idle timeout 120 s
(`COORDINATOR_HTTP_IDLE_TIMEOUT_SECONDS`, `:37`); max request body **16 MiB**
(`:48`); shared `websocket.maxPayloadLength` **4 MiB**
(`COORD_WEBSOCKET_MAX_PAYLOAD_BYTES`, `workers/worker-ws-handler.ts:31`); one
WebSocket object multiplexes both transports on `ws.data.kind` (`:199-226`).
`ECONNRESET` maps to a `Response(null, {status: 0})` with 60 s-batched reset
probes; anything else is `500 {"error":"internal server error"}` (`:263-275,
355-364`).

**The `SessionsPrompt` timeout exception.** Bun caps finite timeouts at 255 s
and `SessionsPrompt` may legitimately wait 300 s, so the listener disables the
idle timer for that one route
(`bun-coordinator-listeners.ts:37-48, 147-169`). A port that copies the
interceptor's `WRITE_METHODS` rule but not the listener policy will have that
RPC reaped behind the 120 s idle timeout.

### 6.1 Compression is OFF on the RPC surface, and that is not an omission

`createConnectRouter` passes **no** `acceptCompression`
(`apps/coord/src/rpc/router.ts:98-106`), verbatim: *"connect-node's
brotli/gzip route through Bun's node:zlib, which SEGFAULTS the whole coord
process (10 crashes 2026-06-27, dumps show node:zlib loaded +
corrupted-pointer address) → workers get 502 / ws-error and can't attach. Same
class as the worker↔coord raw-WS rule: do not run connect-node's zlib
compression under Bun."* `node:zlib` is banned process-wide for the same reason
(`apps/coord/src/gzip-file.ts:3-5`, lint-enforced).

SPA assets **are** gzipped, negotiated on `Accept-Encoding`, for a fixed
extension set (`.js .mjs .css .html .json .svg .map .txt .webmanifest`,
`packages/host/src/spa.ts:12-15`), with `vary: accept-encoding` whenever gzip
is available and a 32-entry memo keyed on (path, mtime, size). The `q=0` case
is handled: `gzip;q=0` correctly disables it (`spa.ts:70-86`). Backups are
gzipped with `CompressionStream`, never `node:zlib`, streaming in 1 MiB slices
into a `.tmp` then renamed.

**In Rust this specific incident cannot recur** — there is no `node:zlib` — but
the decision to keep RPC compression off is worth preserving deliberately, since
`connectrpc` ships `gzip` enabled by default in the workspace pin
(`Cargo.toml:39`). Turning it on changes response framing for every client.

### 6.2 The SPA fallback

`ROOST_WEB_DIST_PATH` names the build output. The source is chosen **once** so
two generations can never mix (`packages/host/src/spa.ts:93-99`): a valid
on-disk `index.html` wins; otherwise the embedded manifest; otherwise
`source: "none"` and every page 404s — with a boot-time
`log.error("main", "spa_source_missing")` because *"A missing SPA source
otherwise presents only as a 404 on every page, which reads like an edge or DNS
fault"* (`main.ts:113-120`).

The fallback rule (`spa.ts:168-183`): non-GET/HEAD → 405. A path that resolves
to a real file is served; a path under `assets/` that does **not** → **404**,
never HTML, because content-hashed bundles must not fall back; anything else →
`index.html`. So **`/s/<id>` deep links work** — they are not files and not
under `assets/`, so they get `index.html`, as do `/t/:fp/*` and `/browse/:fp`.
Path traversal and absolute paths are rejected (`:110-113`).

Caching: `index.html` → `no-cache, no-store, must-revalidate`; `assets/` →
`public, max-age=31536000, immutable`; `fonts/*` → `public, max-age=604800`;
everything else stable → `no-cache`.

### 6.3 Health, metrics, and TLS

**There is no `/health` HTTP route and no `/metrics` route.** Health is (a) the
**public** Connect RPC `MiscHealth` → `{ok, boot_ms, uptime_ms, git_sha}`
(`rpc/handlers-system.ts:104-111`) and (b) `serveServiceHealth`, a
capability-gated UDS / named pipe started **only on `win32`**; darwin and linux
skip it and any other platform throws `unsupported coordinator platform`
(`main.ts:207-227`). Metrics are the **device-authenticated** Connect RPC
`MiscMetrics` (`handlers-system.ts:121-134`).

**TLS is never used by the coordinator.** Header: *"The coordinator serves
plaintext on its loopback bind; the operator's front door owns TLS."*
(`bun-coordinator-listeners.ts:4-5`) The export URL is hard-coded
`http://127.0.0.1:<port>/api/db-export` (`handlers-system.ts:117`), and declared
public origins must be HTTPS (`packages/host/src/config.ts:28-40`).

**The default bind changed in v3**: v2 used `127.0.0.1:4103`
(`packages/host/src/coord-config-schema.ts:13`); v3 uses **`127.0.0.1:4113`**
(`crates/roost-host/src/coord_config.rs:15`, `DEFAULT_COORDINATOR_BIND`) so a v2
and a v3 coordinator can run side by side on one machine. Under
`ROOST_TRUST_PROXY=1` the bind must match `^127\.0\.0\.1:[1-9]\d{0,4}$`, because
trusting `X-Forwarded-For` on a non-loopback socket makes the caller origin
attacker-controlled (`packages/host/src/config.ts:83-91`).

### 6.4 The startup janitor

`apps/coord/src/startup-janitor.ts:9-49`, run once after migrations and tenancy
and **before** any sync feed installs bus listeners. Exactly three statements:

1. `DELETE FROM sessions WHERE status = 'closed'` — closed sessions are deleted,
   not parked. **It never touches `status='open'`**: *"a live long-running
   terminal must never be deleted by a janitor; truly-dead open sessions are
   reconciled by the worker snapshot's ghost-close on reconnect, not by a
   wall-clock age cutoff."* (`:15-19`)
2. delete `workspace_sessions` rows whose workspace has no open session
   (`:20-28`);
3. delete workspaces with no junction rows (`:35-38`).

**There is no time window at all**, and that is deliberate — an age cutoff is
exactly the bug the comment above forbids. Any throw is caught and boot
continues (`:45-47`).

The load-bearing ordering: the janitor runs **before** the sync feeds subscribe,
so its deletes publish **no deltas** — publishing there would be *"a
structurally guaranteed no-op"* (`:1-6, 29-34`). Reconnecting browsers learn
about the pruning from the sync feed's **seed snapshot**, and that seed is what
actually protects the sidebar.

---

## 7. The worker WebSocket

### 7.1 The upgrade

Route `/ws/coord-worker/<64-hex>`, matched by
`/^\/ws\/coord-worker\/([a-f0-9]{64})$/` (`apps/coord/src/workers/worker-ws-upgrade.ts:17`).
The order below is the contract, and each step's position is load-bearing
(`worker-ws-upgrade.ts:37-117`).

| # | Check | On failure |
| ---: | --- | --- |
| 1 | the path is exactly the route and a 64-hex fingerprint | `401 unauthorized` |
| 2 | **the entire query string is empty** | `401 unauthorized`, reason `query_credential` |
| 3 | exactly two subprotocols: `roost-worker-auth`, then a non-empty credential | `401 unauthorized` |
| 4 | the credential verifies | `401 unauthorized`, reason `jwt_invalid` |
| 5 | URL fingerprint = JWT fingerprint = worker principal fingerprint, and the key generation is current | `401 unauthorized`, reason `principal_mismatch` |
| 6 | the runtime upgrade succeeds | `400 upgrade failed` |

**All four refusals answer `401 unauthorized` with one body.** A `400` for a
malformed envelope and a `401` for a bad signature would tell a prober which half
of its guess was right, and this endpoint is reachable by anything that can open a
socket.

**The whole query is rejected, not a denylist.** *"This endpoint has no query
contract. Rejecting the entire query surface guarantees an old `?token=` client
cannot leak a credential into access logs while still authenticating
successfully by subprotocol."* (`worker-ws-upgrade.ts:42-44`.) A denylist would be
a denylist of the spelling v2 shipped. Note this is **per transport**: the Sync
socket legitimately takes `tab`, `since`, `flow` and `sync_v`.

**All five of step 5 must agree.** A valid signature proves a *key*, not a
worker — the same key could be a browser's. The URL is chosen by the dialer, the
JWT is minted by the key holder, and the principal is what the database says the
key **is**. Disagreement between any two means a replay, a stale deployment, or a
stolen key aimed at someone else's machine.

On success the response echoes **only** `roost-worker-auth`, never the credential
(`worker-ws-upgrade.ts:114`), so a proxy logging the handshake learns that a
worker connected and nothing else.

### 7.2 The application barrier

`protocol/spec/worker-link.md` §State machine, restated:

1. the socket is open but **not** application-ready; the only forced first write
   is `WHello`, and a duplicate or mismatched hello closes the socket;
2. `DHelloAck` moves the barrier `hello → replay`; **exactly one** durable
   `WSessionEvent` is in flight, and each exact positive `DEventAck` releases the
   next. A stale or duplicate ACK cannot release it;
3. with no durable event and no blocking reservation, the worker sends one
   authoritative sequenced `snapshot`; the coordinator commits it, ACKs its
   sequence, marks **that exact connection** routable, and repairs missing sessions;
4. the snapshot ACK moves it to `live` and drains the volatile agent-status,
   control, cell and compatibility lanes. A durable append or blocking reservation
   that appears during the snapshot forces replay again;
5. disconnect resets the application state to `hello` and clears the volatile
   lanes, while the durable outbox rows remain. A newer authenticated hello
   immediately supersedes the old connection generation and delayed callbacks are
   identity-fenced.

**The pre-snapshot gate admits only five frame kinds**: `hello`, `event`, `pong`,
`refreshJwt` and the snapshot itself. Anything else is dropped with the diag
`worker-ws.frame_before_snapshot_ready`, **with no ACK**, so the worker replays it
after its snapshot (`worker-ws-handler.ts:222-234`). The comment says why the
socket exists at all before the barrier: *"Keep liveness and durable lifecycle
replay flowing, but do not let terminal metadata or cell frames bypass the
connection-level readiness gate."*

### 7.3 The announced-channel barrier

`opened` **and** `respawned` both bind a new `(worker, channel)` route whose
durable append is still queued. Recognising them synchronously on the socket is
what makes their first cell or metadata frame wait for that binding
(`worker-ws-handler.ts:281-303`). The coordinator-side machine is per channel:

```
Pending ──commit──► Draining ──drained──► (gone)
   │                    │
   └── drop(reason) ────┴──► (gone)
```

| Bound | Value | Why |
| --- | ---: | --- |
| `ANNOUNCED_CHANNEL_MAX_FRAMES` | 64 | `announced-channel-barrier.ts:12` |
| `ANNOUNCED_CHANNEL_MAX_BYTES` | 4 MiB | `:13` |
| `ANNOUNCED_CHANNEL_MAX_MS` | 3,000 | `:14` |

Drop reasons, the complete vocabulary (`:15-22`): `overflow`, `timeout`,
`out_of_order`, `mapping_mismatch`, `superseded`, `append_failed`,
`publish_failed`. Every drop reaches the terminal view hub, which invalidates that
one session's stream (`worker-ws-upgrade.ts:21-28`).

Refusals, in order (`announced-channel-barrier.ts:139-237`):

1. a frame that is neither cell, binary nor terminal metadata → `out_of_order`;
2. a non-positive encoded size, or a non-`compact` metadata frame → `overflow`;
3. 64 frames held, or the byte bound passed → `overflow`;
4. a **non-full** cell whose `seq` is not the exact successor of the last held one
   → `out_of_order`. A `full` frame is exempt *and resets the run*, because a
   full after a gap is precisely the repair that makes the channel usable again;
5. the **socket-wide** work budget refusing the charge → `overflow`. The budget is
   the socket's, shared with the ordered frame queue
   (`worker-ws-upgrade.ts:148`), so one worker opening 64 channels exhausts one
   budget rather than 64.

`commit` refuses when the announcement named a different session (returns false,
channel stays open), and drops with `mapping_mismatch` when the durable index
bound a different session (`worker-ws-handler.ts:104-107` supplies the check).

**Semantic retention** keeps exactly one compact terminal-metadata record per
channel: a title or last-activity fact is semantic state, not a byte stream, and
losing it is not recoverable by replay while the cell stream is invalid anyway
after a cell-loss drop. Pre-announce 3 s, post-drop recovery 30 s, ≤ 64 channels,
≤ 4 KiB per frame (`announced-channel-semantic-retention.ts:13-16`).

### 7.4 The ordered frame queue

Per socket, one bounded async queue preserving arrival order, because the runtime
dispatches messages in order but does not await async handlers. Its budget stays
charged while the handler settles, and it is shared with the announced-channel
barrier.

| Bound | Value | Source |
| --- | ---: | --- |
| `WORKER_FRAME_QUEUE_MAX_FRAMES` | 256 | `worker-frame-queue.ts:5` |
| `WORKER_FRAME_QUEUE_MAX_BYTES` | 16 MiB | `:6` |

Overflow latches every retention owner closed **before** accepting more work, so
a caller cannot keep charging after the close, and then closes `1009 worker queue
overflow` (`:143-145`).

### 7.5 The rate window

`WORKER_DURABLE_EVENT_LIMIT = 600` per `WORKER_DURABLE_EVENT_WINDOW_MS = 60_000`,
per socket (`worker-ws-handler.ts:32-33`). The 601st closes **only that
authenticated worker socket** with `1008 worker event rate exceeded`
(`:259-272`). Closing rather than shedding frames is deliberate: shedding would
leave a hole in the durable log with no way for the worker to know, whereas a
close makes it reconnect and replay everything it never had acknowledged.

The window also rolls on a **backwards** clock step, because a window that
refused to roll would stay permanently exhausted and a permanently exhausted
socket is worse than one wasted window (`:51-58`).

### 7.6 Every close code this transport can emit

| Code | Reason | Meaning |
| ---: | --- | --- |
| 1009 | `worker queue overflow` | the per-socket retained-work budget filled |
| 1008 | `worker event rate exceeded` | 600 durable events in 60 s |
| 1008 | *(protocol violation)* | a dedupe replay carried a different payload |
| 4001 | `revoked` | the key generation moved, or the key was revoked |
| 4003 | `reauth required` | the verified token's deadline passed |
| *(no code)* | default close | a durable append threw, or the socket died — the worker reconnects and replays |

`COORD_WEBSOCKET_MAX_PAYLOAD_BYTES = 4 * 1024 * 1024` is the transport's frame
cap (`worker-ws-handler.ts:31`), shared with the Sync socket.

### 7.7 The limits this transport is given

`protocol/spec/worker-link.md` §Limits. These are the **worker's** constants; the
coordinator enforces its own three in §7.4 and §7.5 above.

| Constant | Value | Source |
| --- | ---: | --- |
| `WORKER_SNAPSHOT_MAX_SESSIONS` | 1,024 | `apps/worker/src/transport/coord-link-constants.ts:39` |
| `WORKER_SNAPSHOT_MAX_BYTES` | 4 MiB | `:40` |
| `UNACKED_CAP` | 8,192 | `:52` |
| `PENDING_BYTES_CAP` | 8 MiB | `:31` |
| `WS_BUFFERED_HIGH_WATER_BYTES` | 4 MiB | `:35` |
| `STABLE_SESSION_MS` | 30,000 | `:48` |
| `STALE_LINK_TIMEOUT_MS` / check | 90,000 / 15,000 | `:61-62` |
| reconnect initial / ordinary cap | 500 / 30,000 | `:4-5` |

**A coordinator-side snapshot bound that is not in that table**:
`MAX_WORKER_SNAPSHOT_SESSIONS = 1,024`, enforced before the transaction opens
(`events/persistence-input.ts:10`, `event-transaction.ts:84-91`), and
`MAX_PERSISTED_UTF8_BYTES = 4_096` per truncated string field
(`persistence-input.ts:7,12-56`), applied *before* any durable write so replay
and projection stay byte-identical.

---

## 8. The Sync WebSocket

### 8.1 The upgrade

Route `/ws/coord-sync`, marker `roost-auth`. The order is the contract
(`apps/coord/src/sync/sync-ws-upgrade.ts:124-213`):

| # | Check | On failure |
| ---: | --- | --- |
| 1 | `Origin`, when present, is allowed | `403 forbidden origin` |
| 2 | exactly two subprotocols: `roost-auth`, then a non-empty credential | `401 unauthorized` |
| 3 | the credential verifies | `401 unauthorized` |
| 4 | the principal is an account device or a worker | `404 not found` for `legacy-self-hosted` |
| 5 | `tab`, when present, is within its byte bound | `400 connection rejected` |

**The origin check runs first, before the credential is read.** A browser can be
made to open a WebSocket to a loopback address from any page it visits, and the
credential rides in a subprotocol the page cannot set. After the credential, a
rejected origin and a rejected credential look identical to the page and it
cannot tell which to fix.

A **missing** `Origin` is allowed: a non-browser client sends none, and it is the
credential that authenticates it. A `null` origin is refused.

Allowed origins are compared exactly (`sync-ws-upgrade.ts:44-67`): the declared
`publicUrl`, the declared `webPublicUrl`, the CORS allowlist, the `https://`
twin of the `Host` header, the worker's loopback SPA origin, and — only when the
bind is loopback — the `http://` twin of the `Host`. *"Exact match only: a prefix
or regex on 127.0.0.1 would admit an attacker's page served from another loopback
port."*

**`legacy-self-hosted` is 404, not 401.** It authenticates. What it lacks is a
**scope**: a Sync feed must know which resources a socket may observe, and a key
with no account has none. `404` says there is nothing here, which is true; `401`
would say the key is not valid, which is false and would send an operator to
re-pair a key that works everywhere else (`:160-170`).

### 8.2 The scope a socket gets

| | browser | worker |
| --- | --- | --- |
| `read_only` | no | **yes** |
| resource scope | the whole install | its own resources |
| `tab_id` | the query value, or `None` | always `None` |
| `viewer_key` | `<fingerprint>:<tab>`, or `None` | always `None` |

*"Worker Sync is a firehose consumer only; it may ACK/subscription-control
delivery but cannot issue terminal view or input commands."* (`:78-83`.) A v2
socket without `tab` is read-only by construction (`:189`).

`flow_control` is on only for the exact `flow=1`; `domain_generations` only for
the exact `flow=1&sync_v=2` (`:180-181`). A socket that did not negotiate flow
control is sequenced by nothing and can never be closed for backpressure: the
client declined the contract, and enforcing it anyway would be inventing a limit
it never agreed to.

### 8.3 The ACK window

Every application frame on a negotiated socket carries a positive, monotonic
`delivery_seq`. Controls use `delivery_seq = 0` and **never consume the window**.

| Bound | Value | Why | Source |
| --- | ---: | --- | --- |
| `APPLICATION_MAX_UNACKED_FRAMES` | 512 | a `flow=1` client's own ack cadence | `sync-ws-v1-delivery.ts:23` |
| `APPLICATION_MAX_UNACKED_BYTES` | 4 MiB | same | `:24` |
| `APPLICATION_ACK_TIMEOUT_MS` | 3,000 ms | same | `:25` |

Both limits are checked **before** the send, and both are inclusive: the 513th
unacknowledged frame closes, and so does the frame that would take the bytes past
4 MiB (`:183-192`). Checking first is what keeps the window from being one frame
over at the moment it closes.

The age deadline is measured from the **oldest** unacknowledged frame, so an
acknowledgement that releases it re-arms the full window for the next one
(`:119-143`).

**Every backpressure path closes `1013 sync backpressure`**, and the five reasons
are the complete vocabulary (`:27-32`): `high_water`, `timeout`, `frame_limit`,
`byte_limit`, `age_limit`. `1013` is "try again later", and a slow client is
exactly that: it reconnects, negotiates, and recovers from the durable log.

The native buffer's high-water mark is a **separate** decision from the
application window, and it is checked *after* a successful send because only then
is there a buffered amount to read (`:216-219`). "The client is behind" and "the
kernel buffer is full" are different problems with different fixes.

### 8.4 The acknowledgement

```
ack > last sent   ->  close 1008 "invalid sync ack"
ack <= highest    ->  harmless, releases nothing
otherwise        ->  release every record at or below it
```

A client cannot have processed a sequence the coordinator never sent, and
continuing would mean the window's accounting no longer describes reality
(`:270-275`). A stale or repeated ack is normal on a socket that also carries
controls, and closing on it would kill healthy clients.

### 8.5 Recovery, and the seed/live gap

If `since > 0` the coordinator fixes a recovery cutoff, replays ordered public
session events above `since` through that cutoff, and then subscribes and advances
to live **without exposing a gap** (`protocol/spec/sync.md:27`). A cursor ahead of
the log requests reset; failed v2 recovery requests reset.

The durable side of that is three reads (§3.8): `getEventMaxId` for the cutoff
captured *after* live subscription, then paged `getEventsThrough` with a 256-row
page, falling back to `getEventsSince(…, 1000)`. This is the path that replaces any
live publication lost to a crash — which is exactly what the pending-publication
store's loss recovery does *not* need to guarantee (§3.6).

A retained seed frame is allowed onto the wire only when prior application work
has been cumulatively acknowledged (`:241-268`). That is ACK-coupled rather than a
fixed chunk, so an arbitrarily large seed cannot outrun the 512/4 MiB window while
live frames remain queued.

A domain's retained snapshot must precede its live frames, and `domain_ready`
closes the snapshot/live gap. `domain_ready` without a current terminal snapshot
token resets that domain with `snapshot_token_invalid`
(`protocol/spec/sync.md:50`).

### 8.6 The retired Connect `Sync`

`CoordinatorService.Sync` stays in the proto *"only to return `Unimplemented` and
direct callers to this endpoint"* (`protocol/spec/sync.md:9`). Callers get **410**
from the fetch layer, not the handler's 501, because a throwing stub still lets
Connect open a response stream and the runtime's abort-listener crash path stays
reachable (`bun-coordinator-listeners.ts:338-340`).

Recovery reset reasons the current code emits include `cursor_ahead_of_log`,
`recovery_failed`, and backfill truncation diagnostics
(`protocol/spec/sync.md:51`).

---

## 9. Spec-versus-code disagreements

Recorded rather than silently resolved, because a spec that quietly disagrees with
its implementation is a spec that will eventually be believed over the code. Each
entry says which side this port follows and why.

### 9.1 The worker link

1. **`protocol/spec/worker-link.md:51` cites the wrong lines for the upgrade
   failures.** It says `apps/coord/src/workers/worker-ws-upgrade.ts:53-117`; the
   whole query ban is at `:42-52` and the subprotocol check at `:53-62`. Not a
   disagreement in substance, only in the pointer. **Followed: the code.**

2. **The spec calls the reconnect constants the link's limits**
   (`worker-link.md:47`). They are the *worker's*, from
   `apps/worker/src/transport/coord-link-constants.ts:4-5`; the coordinator does
   not own them. **Followed: the code, and §7.7 labels them as the worker's.**

3. **The spec does not mention the three coordinator-side bounds this transport
   actually enforces** — the frame queue's 256/16 MiB
   (`worker-frame-queue.ts:5-6`), the announced-channel barrier's 64/4 MiB/3 s
   (`announced-channel-barrier.ts:12-14`), and the 600/60 s rate
   (`worker-ws-handler.ts:32-33`). A reader of the spec alone would size a worker
   link wrongly in both directions. **Followed: the code, and §7.3–§7.5 add them.**

4. **`worker-link.md:29` calls the first forced write `WHello` and says a
   duplicate hello closes the socket, but the spec never says the pre-snapshot gate
   admits only five frame kinds.** The list is in the code
   (`worker-ws-handler.ts:222-228`) and is a security-relevant omission: a reader
   who assumed "everything except cells" would admit control frames before the
   barrier. **Followed: the code, and §7.2 states the five.**

### 9.2 Sync

5. **`protocol/spec/sync.md:41-43` gives the 512/4 MiB/3 s limits as
   `sync-ws-v1-delivery.ts` sources, and that is correct — but the file header
   says those limits are "SHARED with the v2 weighted-lane scheduler, which
   respects the same limits"** (`:8-9`), and the v2 scheduler is a separate file
   (`sync-ws-v2-scheduler.ts`) with its own queue depths this document does not
   transcribe. **Partially undetermined: see §10.**

6. **`sync.md:9` says the Connect `Sync` method "remains in the proto only to
   return `Unimplemented`"**, while the code's fetch layer answers **410** to
   every caller and the handler's 501 is reachable only in a runtime with no such
   guard (`bun-coordinator-listeners.ts:338-340`). The spec's own line 24 records
   the split, so this is under-description rather than contradiction. **Followed:
   the code — §6 and §8.6 both say 410.**

7. **`sync.md:25` says "A v2 socket without `tab` is read-only."** The code
   enforces read-only on a **worker** principal unconditionally, and a browser
   without `tab` gets no `viewer_key` and therefore no socket-bound terminal view
   ownership — which is read-only in effect but a different mechanism
   (`sync-ws-upgrade.ts:171-172,188-190`). **Followed: the code; §8.2 states both
   mechanisms separately.**

### 9.3 Coordinator RPC

8. **`MiscFlags` is declared in the proto and listed in the spec's Messages table
   (`coordinator-rpc.md:22`) as if covered, but no handler exists anywhere in
   `apps/coord/src`.** The spec's catch-all at `:94` scopes only to
   "managed-account, dashboard, coordinator-move/relocation" methods, so
   `MiscFlags` is an unlisted hole. **Followed: the code — `UnwiredInV2`, and
   `tests/method_route_coverage.rs` pins the count of sixteen.**

9. **That same sentence also under-counts.** `AuthMintCoordinatorRelocation` and
   `AuthRedeemCoordinatorRelocation` are relocation methods and are absent from
   `AuthMethods` (`apps/coord/src/auth/handlers-auth.ts:12-27`). Net: **16
   unimplemented methods** (12 auth, 3 coordinator-move, 1 misc), not the 12 a
   reader would tally from the sentence. **Followed: the code.**

10. **"Worker list/deploy message timeout `10,000 ms`" (`coordinator-rpc.md:40`) is
    mislabelled.** `WorkersList` is a pure database read: no worker message, no
    timeout. The `10_000` ms values in `handlers-attachments.ts` are the
    *file/attachment* relays, and the cited range `:51-101` covers only
    `FilesRead`/`ListDir`/`Mkdir`. The real deploy timeout is
    `DEPLOY_TIMEOUT_MS = 1_200_000` (`apps/coord/src/deploy/deploy-jobs.ts:169`)
    and the Windows-update deploy is 15 minutes
    (`deploy/windows-update-deploy-record.ts:16`). **Followed: the code; not
    carried into this port's table because no deploy method is in this slice.**

11. **"File read chunk maximum `4 MiB`" is right but the cited range is not.** The
    value is at `handlers-attachments.ts:68-70`; the same handler uses a **30 s**
    pending-RPC deadline for `FilesReadChunk` (`:86`) against 10 s elsewhere, so
    the spec's 10 s row covers the wrong call. **Followed: the code; not in this
    slice's scope.**

12. **Three line citations drifted.** "Audit page maximum 500 |
    `handlers-system.ts:311-314`" — the cap is at **:313**. "Protected principal
    marker | `auth-interceptor.ts:256-277`" — the marker is at **:256-262**;
    `:264-272` is `requireAccountDevice`, which does not set it. "Terminal-search
    owner | `auth-interceptor.ts:280-293`" — `requireSearchTabId` is
    **:284-295**. "Router service calls exactly 1 | `router.ts:113-137`" and
    "Handler factory spreads 18 | `router.ts:119-136`" are both **exactly
    correct**, as is "RPC path | `coordinator.proto:936-1102`".

13. **The spec scopes itself to `POST /roost.v1.CoordinatorService/<Method>`**
    (`coordinator-rpc.md:9`) and never mentions `/api/db-export`, the Host/Origin
    admission gate, or the health split (a public Connect RPC versus a
    win32-only UDS). That is a scope boundary rather than an error, but a port
    that read only the spec would not know the listener is more than an RPC
    mount. **Followed: the code; §6 documents the whole listener.**

14. **The spec covers the interceptor half of the `SessionsPrompt` fence but not
    the listener half** (`coordinator-rpc.md:30` cites
    `auth-interceptor.ts:114-116`). The runtime independently needs a
    route-specific idle-timeout override for the same reason: the runtime caps
    finite timeouts at 255 s and `SessionsPrompt` may wait 300 s
    (`bun-coordinator-listeners.ts:37-48,147-169`). **Followed: the code; §6
    names it as the one route that disables the idle timer.**

### 9.4 Auth

15. **`auth-and-pairing.md:53` cites the wrong file for close code `4001`.** It
    says *"Key-generation revocation closes with `4001`
    (`apps/coord/src/auth/ws-auth-deadline.ts`)"*. That file contains **only**
    `4003` (`:42`); `4001` lives in `sync-ws-handler.ts:184,359` and
    `worker-ws-handler.ts:156,377`. **Followed: the code; §7.6 and §8 both list
    `4001 revoked` against the two handlers.**

16. **The socket authentication deadline is dead code in v2, and the spec presents
    `4003` as a live mechanism.** `reauthAtMs` defaults to `null`
    (`sync-ws-upgrade.ts:122`) and the only production call site omits it
    (`bun-coordinator-listeners.ts:328`), so `scheduleWsAuthDeadline` never runs
    for a production Sync socket. `verifyJwt` already computes exactly the value
    needed — `validUntilMs` (`jwt.ts:225-228`) — and logs it without wiring it
    (`worker-conn.ts:376-377`). **This port keeps the deadline's mechanism and the
    `4003` close and records the wiring as outstanding: §4.10.**

17. **The spec's Limits table omits the server-side JWT ceiling.** `jwtMaxAgeSecs`
    (default 300, `ROOST_COORDINATOR_JWT_MAX_AGE_SECS`) is the actual expiry bound;
    the table lists only the browser's `JWT_LIFETIME_SECS`
    (`auth-and-pairing.md:36`). **Followed: the code; §4.3 states both and says
    which one is the server's.**

18. **The spec omits three pairing retention bounds**:
    `PAIR_REQUEST_SWEEP_INTERVAL_MS = 60_000`, `BATCH_SIZE = 1_000` and
    `PAIR_REQUEST_TOMBSTONE_MS = DAY_MS` (`auth/pair-request-retention.ts:10-13`).
    **Followed: the code; §4.9 names them.**

19. **"Worker-link rejects any query" is stated globally**
    (`auth-and-pairing.md` §Purpose, via `:25`) but the rule is per transport: the
    worker socket has no query contract at all, while the Sync socket legitimately
    takes four parameters. **Followed: the code; §4.2 and §7.1 both say
    per-transport.**

### 9.5 Session events

20. **`session-events.md:22` says the worker "reserves capacity in the bounded
    SQLite `SessionEventStore` before the corresponding keeper mutation", which is
    true, but the spec gives no hint that the coordinator has a *different*
    bounded thing with the same shape.** Conflating them is the single easiest
    porting mistake here, and it looks correct until the two lifecycles diverge
    on the first crash. **Followed: both, as two named owners; §3 opens with the
    side-by-side table.**

21. **`session-events.md:41` says `SessionEvent.parse` reports Zod validation
    failures.** There is no Zod in Rust, and the equivalent is a
    `roost_protocol` validator returning `ProtocolError` with a field path
    (`crates/roost-protocol/src/error.rs:11-15`). Same information, different
    shape. **Followed: the code's shape.**

### 9.6 Schema and migrations

22. **The spec documents a coordinator bind of `127.0.0.1:4103`**
    (`packages/host/src/coord-config-schema.ts:13`); v3 uses **`127.0.0.1:4113`**
    (`crates/roost-host/src/coord_config.rs:15`) so a v2 and a v3 coordinator can
    run side by side. **Followed: v3. §6.3 says so explicitly, because the export
    URL is derived from the port and a stale 4103 in a port would hand an operator
    a URL nothing answers on.**

---

## 10. What could not be determined

Each item says where I looked and what I found, so a later reader does not repeat
the search.

1. **The v2 Sync **v2 weighted-lane scheduler**'s own queue depths and lane
   weights.** `apps/coord/src/sync/sync-ws-v2-scheduler.ts` is referenced by
   `sync-ws-v1-delivery.ts:8-9` as sharing the 512/4 MiB/3 s limits, but the lane
   queue depths, the weights, and the `domain_ready` ordering inside it are in
   files this slice did not transcribe. **The shared limits are recorded in §8.3
   because the spec and the v1 file both state them; the scheduler's internals are
   not, and §8.5 describes only what `sync-ws-v2-commands.ts:115-147` states
   about `snapshot_token_invalid`.** Looked at:
   `apps/coord/src/sync/sync-ws-v2-scheduler.ts`, `sync-ws-v2-queue.ts`,
   `sync-ws-v2-egress.ts`.

2. **Whether `x-roost-auth-layer: device` is set by the interceptor or by the
   listener in v3.** v2 sets it on the Connect error
   (`apps/coord/src/auth/auth-interceptor.ts:256-262`) and exposes it via
   `access-control-expose-headers` (`apps/coord/src/middleware/security.ts:63`).
   This port sets it on the `ConnectError`'s response headers
   (`crates/roost-coord/src/rpc/service.rs`), which reaches the wire the same way;
   whether the interceptor layer or the handler owns that in the ported shape is a
   Phase-3 assembly question this slice does not settle.

3. **The exact v3 home for `isPublicSessionEvent`.** It is a coordinator-local
   definition in `crates/roost-coord/src/events/visibility.rs` with its intended
   home and its five consumers named in the file. The move to `roost-protocol` is
   not this slice's to make, and making a second copy would be worse than leaving
   one with its consumers listed. **§3.7.**

4. **The full `WRITE_METHODS` list is transcribed; the v2 `AUDIT_SKIP_METHODS` and
   `AUDIT_NEVER_PERSIST_METHODS` are transcribed too**, but the *policy* around
   `PairConfirm` failures is asserted from
   `apps/coord/src/auth/auth-interceptor.ts:152-162` and its comment, and there is
   no separate test-visible name for it in the code beyond the
   `_shouldPersistMethodAudit` seam. Looked at:
   `apps/coord/src/auth/auth-interceptor.ts:135-162`.

5. **The Connect router's compression policy is OFF in v2 for a Bun-specific
   crash reason** (`apps/coord/src/rpc/router.ts:98-106`, "10 crashes
   2026-06-27"). That reason cannot apply to Rust, and the workspace pin enables
   `gzip` by default (`Cargo.toml:39`). **This port has not decided the flag; §6.1
   records the decision as one worth making deliberately rather than by
   inheritance.** Looked at: `connectrpc-0.9.1/Cargo.toml:57-58`,
   `Cargo.toml:39`.

6. **Whether `MAX_WORKER_SNAPSHOT_SESSIONS` (1,024) and the worker's
   `WORKER_SNAPSHOT_MAX_SESSIONS` (1,024) are the same number by intent or by
   coincidence.** They are equal (`events/persistence-input.ts:10` and
   `apps/worker/src/transport/coord-link-constants.ts:39`) and both are 1,024, but
   nothing in either tree says one constrains the other, and they are enforced on
   opposite sides of the socket. **§7.7 records both and does not claim they are
   one bound.**

---

## 11. The file map

### The squashed schema

`crates/roost-coord/migrations/0001_init.sql` — the entire schema in one
generated migration, replayed from v2's 34-file history and verified semantically
identical to it (every table's `PRAGMA table_info`, `foreign_key_list`,
`index_list`, plus every index and trigger's SQL).

### The pure core — no socket, no database, no clock of its own

| File | Owns |
| --- | --- |
| `auth/jwt_claims.rs` | the compact JWS's three segments, the header, the four claims, the audience rules |
| `auth/jwt_crypto.rs` | the Ed25519 signature check, the hand-rolled SPKI prefix, the stored-key length rule |
| `auth/jwt_verify.rs` | the bound ladder: expiry, max-age ceiling, forward skew, `sub == kid`, and the four generation checks |
| `auth/jwt_key_cache.rs` | the cached keys, the TTL, the revocation generations, refresh versus invalidate |
| `auth/principal.rs` | the three principal kinds, the dual-authority refusal, the device/worker predicates |
| `write_gate.rs` | the exclusive keeper-update drain, shared leases, and the two RPC policy lists |
| `events/admission.rs` | the twelve event-admission rules as pure decisions over facts |
| `events/visibility.rs` | the one public/private predicate |
| `events/pending_publications.rs` | the coordinator's bounded publication claim, and the byte-for-byte dedupe comparison |
| `worker_link/upgrade_admission.rs` | the worker's five-step upgrade order and its four refusals |
| `worker_link/announced_types.rs` | the barrier's bounds, phases, drop vocabulary and socket-wide budget |
| `worker_link/announced_barrier.rs` | the barrier machine: announce, enqueue, commit, fail, expire |
| `worker_link/rate_window.rs` | the 600-per-minute durable-event window |
| `sync_ws/upgrade_admission.rs` | the Sync five-step order, the origin policy, the scope a socket gets |
| `sync_ws/ack_window.rs` | the cumulative ACK window and every close it can take |
| `rpc/method_route.rs` | the route table's vocabulary -- domain, auth requirement, status -- and its accessors |
| `rpc/method_route_rows.rs` | the 103 rows, one table per v2 domain folder |
| `rpc/service_impl.rs` | the ONE `impl CoordinatorService` block; all 103 methods, a recorded size exception, and §5.7's four signature facts |

### The thin I/O shell

| File | Owns |
| --- | --- |
| `db.rs` | the pool of one, the nine pragmas, `sqlx::migrate!`, the snapshot and integrity primitives |
| `auth/authorized_keys.rs` | the only place a bearer meets the database: the two joins, the principal decision, the fingerprint helper |
| `auth/authenticate.rs` | the ordered authenticate-one-token path, shared by all three transports |
| `rpc/service.rs` | the service type, the named `Unimplemented` funnel, the permission errors, the two answered methods |
| `http/listener.rs` | the five surfaces, their order, the retired-Sync 410, the bind resolution |
| `http_admission.rs` | the Host/Origin gate and the fail-closed 503 before the port is known |
| `services.rs` | the per-process singletons, built at boot and injected |
| `serve.rs` | `CoordBoot` and the blocking `serve` |

### The tests

| File | Covers |
| --- | --- |
| `tests/jwt_parity.rs` | the primitive against tokens **v2's own signer** minted, plus the four refusals and the SPKI framing |
| `tests/method_route_coverage.rs` | the route table against the proto's service block, in both directions, and the count of sixteen |
| `tests/event_admission.rs` | all twelve admission rules, their order, the oracle property, and the visibility predicate |
| `tests/upgrade_admission.rs` | both upgrade state machines: every refusal, every status, the origin-before-credential order, the query ban |
| `tests/transport_windows.rs` | the announced-channel barrier, the ACK window, the rate window, and the socket-wide budget |
