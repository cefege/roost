<!-- Client-state sync contract: the account-scoped browser keys the coordinator stores and merges across devices. -->
<!-- The wire is protocol/proto/roost/v1/coordinator.proto; the allowlist and limits are crates/roost-protocol/src/client_state.rs. -->

# Client state sync

## Purpose

An operator who opens Roost on another laptop lands where they left off: the
pane layout, the last terminal, the sidebar's recent sessions and manual folder
order, per-folder and per-machine memory, and unsent composer drafts.
The coordinator stores the allowlisted subset, scoped to the account. Browsers
merge it on boot, reconnect and account-scoped live invalidation.

## Surfaces

| Surface | Carrier | Owner | Source |
| --- | --- | --- | --- |
| Wire | `ClientStateList`, `ClientStateSet` (Connect unary, `CoordinatorService`) | Coordinator | `protocol/proto/roost/v1/coordinator.proto` (`ClientStateEntry`, `ClientStateListRequest/Response`, `ClientStateSetRequest/Response`) |
| Allowlist and limits | `is_synced_key`, `MAX_*` constants | Shared by coordinator and browser | `crates/roost-protocol/src/client_state.rs` |
| Storage | `client_state(account_id, key, value, updated_ms)`, primary key `(account_id, key)` | Coordinator | `crates/roost-coord/migrations/{sqlite,postgres}/0015_client_state.sql` |
| Handlers | List, LWW upsert, cap checks | Coordinator | `crates/roost-coord/src/client_state/` |
| Browser store | `ClientStateStore` (`impl KeyValueStore`), debounced flush, boot pull, merge | `roost-client-core` | `crates/roost-client-core/src/client/client_state/` |

## Allowlist

Exact keys: `roost.paneLayout.v1`, `roost.lastSessionByFolder`,
`roost.lastTerminalPath`, `roost.sidebar.recent`,
`roost.sidebar.folderOrder`, `roost.composerDrafts.v1`.

Prefix (the suffix must be non-empty): `roost.lastWorkspaceId.`.

Everything else is device-local and is never sent. In particular the device
key, endpoint selection (`coordinatorUrl`, `coords`, `deploymentMode`,
`localWorkerOrigin`), the per-tab id, pairing and fragment credentials,
theme, and input modality never leave the browser that owns them.

## Limits

| Limit | Value | Enforced |
| --- | --- | --- |
| Key length | 256 bytes | Coordinator refuses |
| Value size | 256 KiB | Coordinator refuses |
| Entries per call | 64 | Coordinator refuses the whole batch |
| Keys per account | 1024 | Coordinator refuses a new key past the cap; the whole batch rolls back |

## Merge rule

Last writer wins on `updated_ms`, the wall-clock epoch milliseconds the
writing browser stamps. The monotonic clock is not used here: it cannot order
writes made on two machines.

- Coordinator upsert: the incoming entry replaces the stored one only when
  `incoming.updated_ms >= stored.updated_ms`. A losing write changes nothing.
- `ClientStateSet` returns the stored entry for every requested key, so a
  browser whose write lost adopts the winner.
- Browser merge (List and Set answers): a remote entry with a newer
  `updated_ms` than the local stamp is applied locally and not marked dirty.
  A local stamp newer than the remote entry marks the key dirty; it is sent on
  the next flush.
- Equal stamps are a tie. The coordinator resolves a tie toward the incoming
  write, so the browser does the same: a dirty local value keeps its place and
  is flushed, and a settled local value takes a differing remote value.
- Non-allowlisted keys from the server are dropped and counted.

Folder discovery writes only `roost.sidebar.observedFolders`, a device-local
append-only ordering cache. An explicit drag writes `roost.sidebar.folderOrder`;
remote manual order takes precedence over locally discovered folders. Activity
never changes either order. An unstamped order from an older browser seeds an
account only when its first successful List has no folder-order entry; otherwise
the existing account order wins.

## Browser timing

- Writes go to the local store immediately and mark the key dirty with a write
  sequence. The client sweep records, on its monotonic clock, the first sweep
  that sees each new sequence. A key flushes once 1000 ms have passed since
  that sweep, in batches of at most 64. The core has no timer of its own.
- A failed `ClientStateSet` parks its keys. A parked key does not retry on the
  timer: it waits for the next List (boot or reconnect) or for a newer local
  write to it.
- A `ClientStateList` is issued on boot and every reconnect. A
  `ClientStateChangedFrame` on Sync v2 requests another List immediately, even
  before domain hydration. Invalidations during a pending List coalesce into
  one trailing pull on the next sweep, so a commit racing that List is not lost.
- `ClientCore::client_state_settled()` is true after the first merged List,
  or after 5000 ms with no answer. The landing page waits on it so an
  offline boot is never blocked.

## Authorization

`ClientStateList` and `ClientStateSet` require a browser account device
(`require_account_device`). The account is the caller's `account_id`, or the
self-hosted tenant account for a legacy-self-hosted browser. Workers are
refused.

## Live delivery

After a successful non-empty Set commits, the coordinator publishes an account
id on the cluster notifier's `roost_client_state` channel. Postgres carries it
across replicas; SQLite delivers in process. Only that account's authenticated
Sync v2 browser sockets that subscribe with `client_state=1` receive the empty
`ClientStateChangedFrame` (field 61). Workers, other accounts and unsubscribed
sockets receive neither the notice nor state values. Notification subscription
is explicit so cached browser builds never receive an unknown control arm.

The frame is unsequenced control traffic, independent of domain readiness and
never retained or replayed. Clients recover missed notices by pulling on
reconnect; notifier reconnect and receiver lag also invalidate the affected
sockets. State values remain behind the account-authorized List RPC.
