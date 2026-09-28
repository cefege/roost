worker/attachments: v2 attachment store, durable operations, reaper, grants and the coordinator attachment arms

Ports apps/worker/src/attachments/attachment-{file-store,file-hash,operation-journal,
operation-receipts,operation-owner,upload,reaper,grants,transfer-admission,transfer-lease}.ts
and browser-command-attachments.ts; composition of main.ts:196-199, attachment-upload.ts:40-43
and boot-local-terminal.ts (attachments::owners::AttachmentOwners, started by one call from
runtime::owners; also builds WAttachDirect's AttachmentDirect). Deletes the unwired, non-v2
attachment_transfer.rs paraphrase and its test.

Replaced downstream arms (owner: attachments::link::AttachmentLink via link_ports::AttachmentLinkPort):
- localAttachmentGrant: was rpc-error "local attachment grants unsupported by this worker" ->
  rpc-ok {grant_id} / rpc-error with the store's message (owners=None keeps the old reply).
- attachmentDirectStatusRequest: was upload_not_found status -> the operation's durable status.
- localAttachmentGrantRevoke: was no-frame warn -> sockets, grants, peers (v2 revokeAttachmentDevice).
- attachmentChunk: was no-frame warn -> relay write; unfenced rpc-ok {abs_path} on the final chunk,
  rpc-error with v2's message on refusal, nothing on progress or on an owner panic (v2 .catch).
  The dead `unowned` helper is removed: all 7 no-frame interim arms now route.

Deliberate deviations (README): a resumed direct upload appends at bytesWritten (v2
attachment-operation-owner.ts:266 "r+" + :374-381 wrote at offset 0); attachments live under
<worker data dir>/attachments, not ~/.roost/attachments (side-by-side reaper/manifest collision,
in-process boot tests would sweep the operator's dir).

Test fixes citing v2: browser_command_attachments delete refuses only / \ .. . (v2 :84-89);
listing test pins distinct mtimes (v2 sorts by mtime_ms); duplicate `x.tar.gz` becomes
`x.tar (2).gz` (v2 attachment-file-store.ts:129-132, Node extname).

Mutations seen red (each reverted): operation_open.rs:169 append->write(at 0) ->
a_resumed_direct_upload_appends_after_the_bytes_it_already_holds; operation_commit.rs:141 dir
fsync dropped -> a_failed_directory_flush_withholds_the_final_receipt; reaper.rs:89 manifest arm
-> files_past_the_ttl_go_and_the_manifest_and_fresh_files_stay; grants.rs:284 cap ->
the_store_holds_at_most_256_grants_but_a_renewal_always_fits; runtime/downstream/attachments.rs:41
Progress->rpc-ok -> a_refused_relay_chunk_is_answered_with_v2s_message; store_paths.rs:53 ->
a_session_id_that_climbs_out_of_the_base_is_refused_and_writes_nothing. transfer_lease
note_valid_activity cap is an equivalent mutant (deadline() already takes the min).
UNVERIFIED (run aborted: roost-keeper failed to compile, then pause): same_carrier, seq check,
unique_name, manifest retain, idle-sweep direct, reaper sort, capability_matches, lease deadline().

Shared edits: link_ports.rs (AttachmentLinkPort + DownstreamOwners.attachments), runtime/owners.rs,
runtime/downstream/mod.rs, runtime/session_stack.rs (SessionStack.attachments; deps() lost its
data_dir param), runtime/deps.rs, runtime/boot_sequence.rs, lib.rs, tests/link_downstream_support,
tests/link_downstream_absent (owners=None helper), tests/browser_command_support, tests/boot_adoption_gate.
No cross-owner crate edits.
