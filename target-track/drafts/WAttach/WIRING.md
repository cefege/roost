# W-ATTACH — phase-2 handoff (self-sufficient)

Slice: W-ATTACH store + transfer (Stage 2W of docs/v3-handoff/roost-v3-finish-and-cutover-plan.md, row "W-ATTACH").
Worktree /home/almalinux/repos/roost-v3-worker (branch recover/workerroot). Lead: agent://WorkerLead2W2.
Sibling that owns the direct/peer half: agent://WorkerLead2W2.WAttachDirect (drafts in target-track/drafts/WAttachDirect/, its own WIRING.md).
State at handoff: every product and test file below is DRAFTED under target-track/drafts/WAttach/ mirroring repo paths.
NO tracked file was edited and cargo was never run: nothing has been compiled. Expect compile fixes.

Rules recap (from the lead's brief, binding): cargo only via `/tmp/wcargo.sh <args>`; compile checks via
`/tmp/wcheck.sh '<regex>'`; files ≤400 lines AFTER rustfmt (the lead runs fmt; measure with
`~/.cargo/bin/rustfmt --edition 2024 --emit stdout <file> | wc -l` — never run `cargo fmt`, never rewrite a
tracked file with rustfmt); `//!` header 3–6 lines naming the v2 file; no unwrap/expect outside tests; a
tracing line per state transition; never commit / git add / stash; mutations must be seen red then reverted.

## 1. What this slice ports (v2 → Rust)

| v2 apps/worker/src/attachments/ | Rust (drafted) |
|---|---|
| attachment-reaper.ts (paths half: attachmentBaseDir, attachmentSessionDir, resolveSessionDirWithinBase, MANIFEST_NAME, ATTACHMENT_OPERATION_DIR_NAME) | src/attachments/store_paths.rs |
| attachment-reaper.ts (sanitizeAttachmentName POSIX branch + Node extname rules) | src/attachments/naming.rs (unit tests inline) |
| attachment-reaper.ts (sweep 24 h TTL / 1 GiB LRU, 1 h interval, boot sweep) | src/attachments/reaper.rs |
| attachment-file-hash.ts | src/attachments/file_hash.rs (+ the one lowercase-hex digest renderer) |
| attachment-file-store.ts | src/attachments/file_store.rs (+ write_private_file / create_private_dir = fs 0600/0700 helpers) |
| attachment-operation-journal.ts | src/attachments/journal.rs (camelCase on-disk JSON, same as v2, so an upgraded worker reads v2 journals) |
| attachment-operation-receipts.ts + ERROR_MESSAGE of attachment-upload.ts | src/attachments/receipts.rs |
| attachment-operation-owner.ts | src/attachments/operation_owner.rs (accept/status/detach/sweep), operation_open.rs (prepare/openLoaded/temp checks), operation_commit.rs (commitFinal/recoverFinalization/failJournal) |
| attachment-upload.ts (facade + 60 s idle sweep) | src/attachments/upload.rs (`AttachmentOperations`, `DirectChunk`, `DirectChunkOutcome`, `RelayChunkOutcome`) |
| attachment-grants.ts | src/attachments/grants.rs (store), grant_checks.rs (validGrantFrame, verify, sameDescriptor, verifyLocalEndpointCapability), grant_listeners.rs (subscribe/notify) |
| attachment-transfer-admission.ts | src/attachments/transfer_admission.rs |
| attachment-transfer-lease.ts | src/attachments/transfer_lease.rs (pure deadline; WAttachDirect drives the timer) |
| attachment-transfer-port.ts | ported by WAttachDirect (src/attachments/transfer_port.rs) by agreement |
| browser-command-attachments.ts | src/browser_commands/attachments.rs (REPLACES the tracked file) |
| transport/coord-link-downstream.ts `attachmentChunk`, coord-link-deps.ts `onAttachmentChunk`, coord-link-direct-terminal.ts `localAttachmentGrant`/`localAttachmentGrantRevoke`/`attachmentDirectStatusRequest`, coord-link-direct-deps.ts `onAttachmentDirectStatusRequest`, boot-local-terminal.ts `revokeAttachmentDevice` | src/runtime/downstream/attachments.rs (arms) + src/attachments/link.rs (`AttachmentLink`, the port impl) |
| main.ts:196-199 `startAttachmentReaper()` | owners.rs edit below |

Not ported: win32 branches of sanitize/short-path/dir-fsync (Windows paused). v2's unused private
`matchesExpectedMetadataPeer` (dead in v2). v2 per-grant expiry setTimeout: expiry is lazy (swept on
install/current/verify/authorize_peer; `Removed{Expired}` announced once then); the only listener (direct
sockets) ignores Expired exactly as v2 does, so nothing observable changes.

## 2. Draft files → move into place (cp drafts/WAttach/crates/... → crates/...)

New: src/attachments/{file_hash,file_store,grant_checks,grant_listeners,grants,journal,link,naming,operation_commit,operation_open,operation_owner,reaper,receipts,store_paths,transfer_admission,transfer_lease,upload}.rs,
src/runtime/downstream/attachments.rs,
tests/attachment_support/mod.rs, tests/{attachment_operation_owner,attachment_operation_recovery,attachment_upload,attachment_transfer_lease,attachment_reaper,attachment_grants,link_downstream_attachments}.rs.
Replace whole (tracked): src/attachments/mod.rs (has WAttachDirect's 15 `pub mod` lines already — agreed; they must NOT edit mod.rs), src/browser_commands/attachments.rs.
Delete (cutover; a paraphrase with no production caller, not v2 — 300 s lease, per-document bound the worker never enforced): src/attachment_transfer.rs, tests/attachment_transfer.rs, and in src/lib.rs the line `pub mod attachment_transfer;`.
Formatted sizes (rustfmt): grants 367, operation_owner 375, journal 288, file_store 263, others < 240. Test files format to ≤ 260 lines each (measured without the shared support module).

## 3. Exact edits to existing files (re-read each right before editing; siblings edit concurrently)

1. src/link_ports.rs — add (imports: `roost_proto::{AttachmentTransferStatus, DAttachmentChunk, DAttachmentDirectStatusRequest, DLocalAttachmentGrant}`, `crate::attachments::upload::RelayChunkOutcome`):
```rust
/// Relayed attachment uploads and the direct-carrier grants the coordinator
/// installs. v2 `onAttachmentChunk`, `onLocalAttachmentGrant`,
/// `onLocalAttachmentGrantRevoke`, `onAttachmentDirectStatusRequest`.
pub trait AttachmentLinkPort: Send + Sync + std::fmt::Debug {
    /// The chunk is written in this call (arrival order is write order); the
    /// future settles what the coordinator is told.
    fn accept_relay_chunk(&self, chunk: DAttachmentChunk) -> OwnerFuture<RelayChunkOutcome>;
    /// `Err` is the message v2's `rpc-error` carries.
    fn install_grant(&self, request: &DLocalAttachmentGrant) -> Result<(), String>;
    fn revoke_device(&self, device_fingerprint: &str);
    fn direct_status(&self, request: &DAttachmentDirectStatusRequest) -> AttachmentTransferStatus;
}
```
   and field `pub attachments: Arc<dyn AttachmentLinkPort>,` in `DownstreamOwners` (WAttachDirect adds its own `attachment_peer` field/port).
2. src/runtime/downstream/mod.rs — add `mod attachments;` next to `mod owner_task;` and replace these four arms (old → new):
   - `LocalAttachmentGrant(request) => link.reply(replies::rpc_error(request.request_id, replies::LOCAL_ATTACHMENT_GRANTS_UNSUPPORTED))` → `CoordWorkerDownstream::LocalAttachmentGrant(request) => self.attachment_grant(request, link),`
   - `AttachmentDirectStatusRequest(request) => link.reply(replies::attachment_status_unavailable(&request))` → `=> self.attachment_status(request, link),`
   - `LocalAttachmentGrantRevoke(_) => unowned(kind, "the local attachment grant owner")` → `LocalAttachmentGrantRevoke(request) => self.attachment_grant_revoke(&request),`
   - `AttachmentChunk(_) => unowned(kind, "the attachment upload owner")` → `AttachmentChunk(chunk) => self.attachment_chunk(chunk),`
   The owners=None branches inside the new methods keep v2's absent answers, so `replies::LOCAL_ATTACHMENT_GRANTS_UNSUPPORTED` and `replies::attachment_status_unavailable` stay used. Report this table to the lead (arm → old verbatim reply → new owner `attachments::link::AttachmentLink`).
3. Attachment base = v2 `~/.roost/attachments` (was `<data_dir>/attachments`). src/runtime/session_stack.rs: replace `pub fn attachment_root(data_dir) -> PathBuf` with a `pub attachments: AttachmentBase` field on `SessionStack`, computed once in `build` as `AttachmentBase::new(attachment_base_dir(&home))` where `home = roost_host::env::ProcessEnv::new().home_dir()` (EnvSource trait); if HOME is unset fall back to `data_dir.join("attachments")` with a `tracing::warn!` naming the fallback (v2's os.homedir() never fails). Update the existing `attachments_root = …` log line to `stack.attachments.root()`. `SessionStack::deps(..)` passes `self.attachments.clone()`; drop its `data_dir` param if now unused and fix the call in runtime/boot_sequence.rs (`stack.deps(&boot.data_dir, …)`; boot_sequence must stay ≤400).
   src/runtime/deps.rs: `WorkerCapabilities.attachment_root: PathBuf` → `attachments: AttachmentBase`; `attachments: Arc::new(SessionAttachments::new(attachments))` (constructor lost its unused `platform` arg).
4. src/runtime/owners.rs (`WorkerOwners::build`, before `let downstream = DownstreamOwners {`):
```rust
// v2 main.ts:196-199 and attachment-upload.ts:40-43: one operation owner for
// every carrier, its 60 s idle sweep, and the reaper's boot + hourly sweep.
let attachment_operations = AttachmentOperations::new(stack.attachments.clone(), system_clock());
let attachment_idle_sweep = attachment_operations.spawn_idle_sweep();
let attachment_reaper = start_attachment_reaper(stack.attachments.clone());
let attachment_grants = Arc::new(AttachmentGrantStore::system(process_epoch));
// WAttachDirect: let attachment_direct = AttachmentDirect::new(AttachmentDirectDeps { grants: Arc::clone(&attachment_grants), operations: attachment_operations.clone(), worker_fingerprint, worker_epoch, peer, native_loader, coordinator_generation });
let attachments = AttachmentLink::new(attachment_operations, Arc::clone(&attachment_grants), attachment_direct.clone());
```
   field `attachments: Arc::new(attachments) as Arc<dyn AttachmentLinkPort>,` in the DownstreamOwners literal; keep the two JoinHandles on `WorkerOwners` and `abort()` both in `shutdown` (with a tracing line); if WAttachDirect's dispose does not call `attachment_grants.dispose()`, call it in shutdown (v2 disposeDirect order: sockets, peer owner, grants). Imports: `crate::attachments::{system_clock, grants::AttachmentGrantStore, link::AttachmentLink, reaper::start_attachment_reaper, upload::AttachmentOperations}`, `crate::link_ports::AttachmentLinkPort`. owners.rs is 182 lines now.
5. tests/link_downstream_support/mod.rs — `Fakes` implements the port and `owners()` fills the field:
```rust
impl AttachmentLinkPort for Fakes {
    fn accept_relay_chunk(&self, chunk: DAttachmentChunk) -> OwnerFuture<RelayChunkOutcome> {
        self.log.push(format!("attachment_chunk:{}", chunk.request_id));
        self.settle(RelayChunkOutcome::Progress)
    }
    fn install_grant(&self, request: &DLocalAttachmentGrant) -> Result<(), String> {
        self.log.push(format!("attachment_grant:{}", request.grant_id));
        Ok(())
    }
    fn revoke_device(&self, device_fingerprint: &str) {
        self.log.push(format!("attachment_revoke:{device_fingerprint}"));
    }
    fn direct_status(&self, request: &DAttachmentDirectStatusRequest) -> AttachmentTransferStatus {
        self.log.push(format!("attachment_status:{}", request.upload_id));
        AttachmentTransferStatus { upload_id: request.upload_id.clone(), ..Default::default() }
    }
}
```
   and `attachments: Arc::clone(&fakes) as Arc<dyn AttachmentLinkPort>,` in `owners()` (before the final `lifecycle: fakes as …` move).
6. tests/link_downstream_absent.rs — `answers()` dispatches WITH owners and asserts no owner was called, so the four attachment rows move to a new owners=None helper (another slice may add the same helper — re-read first):
```rust
/// v2's answers when the `CoordLinkDeps` attachment callbacks are absent.
fn answers_without_owners(frame: Down) -> Vec<Up> {
    let (uplink, mut receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, EPOCH, None);
    let mut link = FakeLink::default();
    dispatcher.dispatch(frame, Instant::now(), &mut link);
    assert!(receiver.try_recv().is_none(), "nothing is answered later");
    link.replies
}
```
   Rows: `LocalAttachmentGrant` (rpc-error "local attachment grants unsupported by this worker"), `AttachmentDirectStatusRequest` (the upload_not_found status block), and `LocalAttachmentGrantRevoke` + `AttachmentChunk` in the inert list → all via `answers_without_owners`; assertions unchanged.
7. tests/browser_command_support/mod.rs — `SessionAttachments::new(AttachmentBase::new(root.join("attachments")))` (import `roost_worker::attachments::store_paths::AttachmentBase`; drop the `HostPlatform::Linux` arg).
8. tests/browser_command_attachments.rs — `a_delete_refuses_a_filename_that_is_not_a_leaf`: remove `".roost-manifest.json"` from the refused list and the final `assert!(manifest.is_file(), …)` (v2 browser-command-attachments.ts:84-89 refuses only `/`, `\`, `..`, `.`; v2 wins). Other tests there should pass unchanged (listing now skips only the manifest and non-files, sorted by fractional mtime desc — v2's).
9. crates/roost-worker/README.md does not exist yet: create it (or, if WAgentsReport created it, re-read and append) with a section `## Deliberate deviations from v2` and the line:
   `- Attachment resume appends at the journal's bytesWritten. v2 reopened a parked temp with "r+" (apps/worker/src/attachments/attachment-operation-owner.ts:266) and writeAllSync (:374-381) wrote without a position, i.e. at offset 0, so every resumed direct upload overwrote its own head while the manifest recorded the intended digest. Guard: tests/attachment_operation_owner.rs a_resumed_direct_upload_appends_after_the_bytes_it_already_holds.`

## 4. Tests (v2 → Rust) and the run commands

- v2 tests/attachments/attachment-operation-owner.test.ts → tests/attachment_operation_owner.rs (carrier fence, durable duplicate receipt, progress+relay commit bytes, idle sweep; + new resumed-append test) and tests/attachment_operation_recovery.rs (final-name collision, status never reads bytes — temp chmod 000 replaces v2's readSync spy, failed POSIX dir flush — op dir chmod 0300 replaces v2's fs.promises.open mock and self-skips if the user can open it, journal shape). v2's "never blocks the event loop on fsync" spy assertions are not portable (no fsyncSync spy); its behaviour half is ported.
- v2 attachment-upload.test.ts → tests/attachment_upload.rs (relay facade) + naming.rs unit tests (sanitizer; its win32 half not ported).
- v2 attachment-transfer-lease.test.ts → tests/attachment_transfer_lease.rs.
- No v2 test for reaper/grants: tests/attachment_reaper.rs (TTL, manifest survives, shortcut ages with target, sparse-file 1 GiB LRU), tests/attachment_grants.rs (install validation, 256 cap + renewal, hello admission, peer tuple, lazy expiry announced once, replacement/revocation announcements).
- Acceptance ("a chunk over the coordinator link lands a file with v2's naming/manifest"): tests/link_downstream_attachments.rs (Dispatcher + real AttachmentLink + real AttachmentDirect with peer disabled; needs WAttachDirect's `direct_owners::{AttachmentDirect, AttachmentDirectDeps}` and WPeer's `peer::{PeerTransportConfig, native::str0m_loader, coordinator_generation::CoordinatorGeneration}` — adjust names to what actually landed).
- Unit tests inline: store_paths (traversal), naming (4), file_hash (empty digest).
- v2 attachment-direct-socket.test.ts / attachment-peer-owner.test.ts belong to WAttachDirect.
Run: `/tmp/wcargo.sh test -p roost-worker --test attachment_operation_owner --test attachment_operation_recovery --test attachment_upload --test attachment_transfer_lease --test attachment_reaper --test attachment_grants --test link_downstream_attachments --test link_downstream_absent --test browser_command_attachments` and `/tmp/wcargo.sh test -p roost-worker --lib attachments`, then `/tmp/wcargo.sh clippy -p roost-worker --all-targets -- -D warnings`.

## 5. Mutations planned (make each, see the named test fail, revert; report file:line)

1. operation_open.rs `resume_temp`: `.append(true)` → `.write(true)` (v2 write-at-0) → attachment_operation_owner::a_resumed_direct_upload_appends_after_the_bytes_it_already_holds.
2. operation_open.rs `prepare`: drop `|| !existing.journal.same_carrier(..)` / the Loaded-branch same_carrier → a_coordinator_continuation_of_a_direct_upload_is_refused_and_only_status_survives.
3. operation_owner.rs `advance`: drop `|| u64::from(chunk.seq) != journal.next_seq` → attachment_upload::an_out_of_order_chunk_aborts_the_upload_and_removes_its_temp.
4. file_store.rs `unique_name`: return `sanitized.to_owned()` unconditionally → the_original_name_is_kept_and_duplicates_get_a_numbered_suffix.
5. file_store.rs `record_attachment_hash`: delete the `retain` line → recording_a_name_with_new_content_prunes_its_stale_digest.
6. operation_commit.rs `flush_commit`: remove `sync_attachment_directory_async(&operation_dir)` from the second try_join → a_failed_directory_flush_withholds_the_final_receipt.
7. operation_owner.rs `sweep_idle`: fail direct operations too → the_idle_sweep_fails_a_silent_relay_and_parks_a_silent_direct_upload.
8. reaper.rs: remove the `Some(MANIFEST_NAME) => {}` arm → files_past_the_ttl_go_and_the_manifest_and_fresh_files_stay; sort survivors newest-first → the_base_is_held_under_the_cap_by_evicting_the_oldest_survivors.
9. grants.rs `install_locked`: drop the MAX_GRANTS check → the_store_holds_at_most_256_grants_but_a_renewal_always_fits.
10. grant_checks.rs `capability_matches`: return true → a_hello_is_admitted_only_with_the_exact_descriptor_and_secret.
11. transfer_lease.rs `note_valid_activity`: `self.idle_deadline = now + IDLE` (no hard min) → valid_activity_refreshes_idle_but_never_extends_the_hard_lease.
12. runtime/downstream/attachments.rs: answer `Progress` with rpc-ok → link_downstream_attachments::a_refused_relay_chunk_is_answered_with_v2s_message.
13. store_paths.rs `resolve_session_dir`: skip `starts_with` → a_session_id_that_climbs_out_of_the_base_is_refused_and_writes_nothing (+ store_paths unit test).

## 6. Interfaces agreed with WAttachDirect (they consume; do not rename)

`attachments::{AttachmentClock = Arc<dyn Fn() -> Instant + Send + Sync>, system_clock(), Carrier, OperationDescriptor}`;
`upload::AttachmentOperations` (Clone): `new(AttachmentBase, AttachmentClock)`, `async accept_direct_chunk(&self, DirectChunk) -> DirectChunkOutcome` (does v2 syncAttachmentOperationProgress; flush error → Failed(WriteFailed)), `status(&self, session_id, upload_id) -> AttachmentOperationStatus`, `detach_direct_carrier(&self, carrier_id)`, `spawn_idle_sweep()`, `accept_relay_chunk(DAttachmentChunk) -> OwnerFuture<RelayChunkOutcome>`;
`DirectChunk{upload_id, session_id, filename, short_path, total_bytes: u64, data: Vec<u8>, last, seq: u32, offset: u64, chunk_sha256, carrier_id}`; `DirectChunkOutcome{Progress(receipt), Committed{abs_path, receipt}, Failed(AttachmentOperationError)}`;
`receipts::{AttachmentOperationReceipt{seq: u32, next_seq: u32, bytes_received: u64, chunk_sha256, committed, abs_path}, AttachmentOperationStatus{upload_id, next_seq: u32, bytes_received: u64, last_chunk_sha256, committed, abs_path, error: Option<AttachmentOperationError>} + error_str() + to_proto(), AttachmentOperationError{..7} + as_str()/message()/reason() -> roost_protocol::attachment_transfer::TransferErrorReason}`;
`grants::{AttachmentGrantStore::new(epoch, clock)/system(epoch), install(&DLocalAttachmentGrant) -> Result<AttachmentGrant, String>, current(&str), verify(&AttachmentGrantCredential) -> Result<AttachmentGrant, &'static str>, authorize_peer(&PeerGrantRequest) -> PeerGrantAuthorization{Authorized, GrantUnavailable, Expired}, revoke_device(&str) -> usize, subscribe(GrantListener) -> GrantSubscription (cancel(self); drop does not unsubscribe), dispose(); GrantChange{Installed{grant, previous}, Removed{grant, reason: GrantRemovalReason{Expired, Revoked, Cleared, Disposed}}}}` — listeners run with NO store lock held;
`transfer_admission::{AttachmentPeerExpectedTuple, AttachmentUploadMetadata, admit_attachment_transfer_hello(&AttachmentTransferHello, &AttachmentGrantStore, Option<&AttachmentPeerExpectedTuple>) -> Result<_, TransferErrorReason>, attachment_metadata_matches_grant}`;
`transfer_lease::AttachmentTransferLease{start(now), allows_activity(&mut, now) -> bool (latches), note_valid_activity(&mut, now) -> bool, deadline()}`;
`store_paths::{AttachmentBase::new(root), root(), session_dir(id), resolve_session_dir(id), attachment_base_dir(home), MANIFEST_NAME, ATTACHMENT_OPERATION_DIR_NAME, SHORTCUT_DIR_NAME}`.
From WAttachDirect I use: `direct_owners::AttachmentDirect` (Clone, Debug) with `revoke_sockets(&str)` and `revoke_peers(&str)`; roost-protocol `attachment_transfer::{DIRECT_CHUNK_BYTES, ACTIVE_MAX_MS, IDLE_MS, GRANT_TTL_MS, TransferErrorReason, is_chunk_sha256}` (their cross-owner roost-protocol module; must land before or with this).

## 7. Decisions (lead-approved) and open points

- APPROVED (integrator ruling): resumed upload appends at bytesWritten — named v2-defect fix; README line (edit 9); commit body cites attachment-operation-owner.ts:266 and :374-381; the resumed-bytes test must be seen red with mutation 1.
- My call, report it: attachment base moved to v2's `~/.roost/attachments` (edit 3), because v2 wins and agents are told these paths; fallback to `<data_dir>/attachments` only when HOME is unset.
- My call, report it: browser `delete-attachment`/`list-attachments` aligned to v2 (edit 8; list includes dotfiles other than the manifest; mtime_ms fractional).
- Relay replies go out UNFENCED (`uplink.send`), as v2's `link().send`; the other arms reply in receive order via `link.reply`. A panicking relay owner answers rpc-error with the panic message (v2 has no such path).
- Relay chunks carry no digest: `chunk_sha256: None` → the owner hashes once (v2 hashed twice to the same result).
- std `Mutex` with `unwrap_or_else(PoisonError::into_inner)` (crate convention; parking_lot is not a dependency).
- Open: macOS directory fsync uses `File::sync_all` (F_FULLFSYNC) — v2 used plain fsync; Linux is the gated platform.
- Open: the lead said to report consumers: RelayChunkOutcome → downstream/attachments.rs; DirectChunkOutcome, GrantChange, PeerGrantAuthorization, AttachmentUploadMetadata, lease.deadline(), receipts → WAttachDirect's direct_sockets/peer owner; status.to_proto → link.rs + direct sockets; SweepSummary → the reaper's own log line; CommittedAttachmentDestination.verified_existing_file → operation_commit::recover_finalization; AttachmentProbe → browser_commands probe.

## 8. Commit message draft (the lead commits)

`worker/attachments: v2 attachment store, durable operations, reaper, grants and the coordinator attachment arms`
Body: ports apps/worker/src/attachments/attachment-{file-store,file-hash,operation-journal,operation-receipts,operation-owner,upload,reaper,grants,transfer-admission,transfer-lease}.ts and browser-command-attachments.ts; routes the attachmentChunk, localAttachmentGrant, localAttachmentGrantRevoke and attachmentDirectStatusRequest downstream kinds to attachments::link::AttachmentLink (owners=None keeps v2's absent answers); starts the reaper (boot + hourly, 24 h / 1 GiB) and the 60 s idle sweep from runtime::owners; base is v2's ~/.roost/attachments. Deliberate deviation: a resumed upload appends at bytesWritten (v2 attachment-operation-owner.ts:266 "r+" + :374-381 write without position overwrote the head). Deletes the unwired attachment_transfer.rs paraphrase. Mutations: <fill from §5 results>. Consumers: <§7>.
Paths: every file in §2 and §3.
