# v2 worker → Rust map (scout WorkerMap, tree b0d026bc)

## Architecture

v3 splits v2's worker across these crates:
- `roost-worker`: runtime (boot and link), session, keeper_pool, browser_commands, event_store, host, capture.
- `roost-keeper`: the daemon plus the client that ports keeper/*.ts.
- `roost-protocol`: wire types, proto adapters, keeper_update contract, terminal_capture/search/input, local_ui_door, terminal_peer framing.
- `roost-term`: the alacritty-based core replacing wterm. It has no reply queue and no synchronizedOutput.
- `roost-host` and `roost-observability`: paths, env and clock.

In production, boot_sequence wires: identity → enroll → door bind → outbox → reconcile and keeper admission → session_stack → link (browser-command pump into Deps) → adoption → readiness. The session layer ingests keeper output into the core and ring, but nothing ticks emission or drains raw metadata. Downstream frames other than browser-command, ack and ping are dropped with a warning, so input, stream-state/resize, grants, peer and keeper-update have no handlers.

## Table

# v2 worker → v3 Rust map

Rust paths are relative to `crates/roost-worker/src/` unless prefixed:
- `keeper:` = `crates/roost-keeper/src/`
- `proto:` = `crates/roost-protocol/src/`
- `term:` = `crates/roost-term/src/`
- `hostc:` = `crates/roost-host/src/`
- `obs:` = `crates/roost-observability/src/`

"unwired" means the code exists but has no production caller.

## root
| v2 path | status | rust path(s) | missing |
|---|---|---|---|
| main.ts | PARTIAL | runtime/mod.rs, runtime/boot_sequence.rs, bin/roost-worker.rs | No heartbeat, OMP integration install, agent report server, attachment sweep, or local-terminal owners |
| shell-spec.ts | PORTED | shell_spec.rs, host/shell_spec_resolver.rs, host/shell_bootstrap.rs, keeper_pool/spawn_spec.rs | win32/PowerShell dropped by design |
| snapshot.ts | PORTED | runtime/snapshot_source.rs | – |
| session-scrollback-ring.ts | PORTED | session/ring.rs | – |
| file-rpcs.ts | PORTED | browser_commands/file_commands.rs | – |
| fsm.ts | PORTED | channel_fsm.rs | – |

## util
| v2 path | status | rust path(s) | missing |
|---|---|---|---|
| util/mono.ts | PORTED | obs:clock.rs (EventClock::mono_ns); SessionManager clock | – |
| util/path.ts | PORTED | session/spawn.rs (canonical_session_cwd, expand_home), session/stream_scan.rs (parse_osc7_worker_path), browser_commands/file_commands.rs, browser_commands/attachments.rs | Windows path forms dropped by design |

## transport
| v2 path | status | rust path(s) | missing |
|---|---|---|---|
| coord-client.ts | PARTIAL | runtime/bootstrap_redeem/{mod,register}.rs, runtime/reconcile.rs | No heartbeat RPC |
| coord-link.ts | PORTED | runtime/link_loop.rs, link_dial.rs, runtime/link_serve.rs, runtime/credential.rs, runtime/link_loop/reconnect_loop.rs | – |
| coord-link-agent-status.ts | PARTIAL | runtime/link_loop/volatile.rs (send_agent_status) | One record per session only (no retirement history); no producer calls it |
| coord-link-codec.ts | PORTED | runtime/link_wire.rs, proto:proto_adapters/coord_worker_proto* | – |
| coord-link-constants.ts | PORTED | backoff.rs, outbox.rs | – |
| coord-link-deps.ts | PARTIAL | runtime/deps.rs, runtime/session_stack.rs | No agent registry and no direct-terminal owners |
| coord-link-direct-deps.ts | NONE | – | Direct grant/peer/route wiring |
| coord-link-direct-terminal.ts | NONE | – | Peer answer/route result/probe controls |
| coord-link-downstream.ts | PARTIAL | runtime/link_drain.rs:on_frame | Only HelloAck, Ping, BrowserCommand and EventAck are handled; the other 24 variants are dropped with a warning |
| coord-link-input-authority.ts | NONE | – | Sync-input authority |
| coord-link-keeper-update.ts | PARTIAL | session/control_lanes.rs (keeper_update_prepared), session/keeper_admission.rs | KeeperUpdatePrepare is unhandled |
| coord-link-native-writer.ts | PORTED | runtime/link_serve.rs, runtime/link_drain.rs | bufferedAmount sampling not applicable |
| coord-link-outbox.ts | PORTED | outbox.rs, runtime/link_drain.rs | – |
| coord-link-reconnect.ts | PORTED | runtime/reconnect.rs, backoff.rs, runtime/link_loop/reconnect_loop.rs | – |
| coord-link-replay-barrier.ts | PORTED | link_barrier.rs, runtime/link_loop/durable.rs | – |
| coord-link-terminal-metadata.ts | PORTED | runtime/link_loop/volatile.rs (send_terminal_metadata, merge_terminal_metadata) | No producer calls it |
| coord-link-terminal-results.ts | NONE | – | Terminal-control result truth mapping |
| coord-link-types.ts | TYPES | proto:wire/coord_worker{,/upstream,/downstream,/payloads}.rs | – |
| coord-link-unacked.ts | PORTED | link_barrier.rs (Pump), runtime/link_loop/durable.rs | – |
| coord-link-windows-update.ts | NONE | – | Windows not ported |
| event-sink.ts | PORTED | session/sinks.rs, session/journal_sink.rs | – |
| heartbeat.ts | NONE | (samplers in host/samples.rs, unwired) | Whole heartbeat loop |
| session-event-store.ts | PORTED | event_store.rs, event_store/admission.rs | – |
| session-event-store-database.ts | PORTED | event_store/database.rs, database/rows.rs, database/claims.rs | – |
| session-event-store-errors.ts | TYPES | event_store.rs ReserveError, event_store/database.rs JournalError | – |
| session-event-store-limits.ts | TYPES | event_store.rs MAX_ROWS, MAX_PAYLOAD_BYTES, MAX_DATABASE_BYTES | – |
| session-event-store-schema.ts | PARTIAL | event_store/database/schema.rs | A v1 file is refused rather than migrated to v2 |
| session-event-store-sequence.ts | PARTIAL | event_store/database/sequence.rs | Retired text-watermark import absent |

## terminal
| v2 path | status | rust path(s) | missing |
|---|---|---|---|
| terminal-core-capacity.ts | NONE | report types only: proto:wire/worker (TerminalCoreCapacityReport), proto:proto_adapters/terminal_core_capacity_proto.rs | Admission/lease owner |
| terminal-input-route-owner.ts | NONE | – | Input-route fencing |
| terminal-input-work-budget.ts | NONE | – | Input work budget |
| terminal-pipeline-snapshot.ts | NONE | – | Pipeline evidence |
| terminal-pipeline-snapshot-bounds.ts | NONE | – | Pipeline bounds |
| terminal-query-reply.ts | NONE | term:lib.rs says getResponse/writeRaw are absent | Probe tokenizer and ordered reply lane |
| terminal-replay-align.ts | NONE | – | UTF-8/escape cut alignment on evicted replay |
| terminal-stream-scan.ts | PORTED | session/stream_scan.rs, session/agent_osc.rs | Windows OSC 7 forms dropped |
| view/terminal-view-owner.ts | NONE | – | View owner (capabilities.rs) |
| view/terminal-view-owner-screen.ts | NONE | – | – |
| view/terminal-view-owner-streams.ts | NONE | – | – |
| search/terminal-search.ts | PORTED | browser_commands/search_scan.rs | – |
| search/terminal-search-batch.ts | PORTED | browser_commands/search_scan.rs, browser_commands/search.rs | – |
| search/terminal-search-cancellation.ts | PORTED | browser_commands/search_cancellation.rs | – |
| search/terminal-search-matcher.ts | PORTED | browser_commands/search_match.rs | – |
| search/terminal-search-result.ts | PORTED | browser_commands/search_page.rs | – |
| search/terminal-search-scheduling.ts | PORTED | browser_commands/search_scan.rs (lock released per slice) | – |
| peer/terminal-packet-port.ts | NONE | – | – |
| peer/terminal-peer-connection.ts | NONE | – | WebRTC |
| peer/terminal-peer-history-reservation.ts | NONE | – | – |
| peer/terminal-peer-native.ts | NONE | – | – |
| peer/terminal-peer-native.generated.ts | NONE | – | Generated stub with no content elsewhere |
| peer/terminal-peer-owner.ts | NONE | – | – |
| peer/terminal-peer-packet-budget.ts | NONE | framing/bounds live in proto:terminal_peer/* | Worker accounting |
| peer/terminal-peer-packet-port.ts | NONE | proto:terminal_peer/packets.rs, packet_queue.rs (framing only) | – |
| peer/terminal-peer-packet-test-fault.ts | NONE | – | – |
| peer/terminal-peer-request-validation.ts | NONE | – | – |
| peer/terminal-peer-test-faults.ts | PARTIAL | peer/mod.rs (OfferFault enum) | No fault injection sites |

## session
| v2 path | status | rust path(s) | missing |
|---|---|---|---|
| session-cell-scheduler.ts | PARTIAL | session/cell_scheduler.rs | No runtime timer drives emit |
| session-cell-sinks.ts | PORTED | session/cell_sink.rs, runtime/link_loop/cell_sink.rs | – |
| session-channel-creation-gate.ts | PARTIAL | session/control_lanes.rs (keeper_update_prepared write freeze) | No spawn lease that waits for in-flight creations |
| session-constants.ts | TYPES | session/cell_scheduler.rs (INPUT_ECHO_WINDOW_MS replaces MAX_PENDING_INPUT_ECHO_PROMOTIONS), session/stream_scan.rs (MODE_CARRY_MAX), session/raw_metadata.rs, session/binding.rs (RESUME_STAGE_CAP_BYTES) | – |
| session-control-lanes.ts | PORTED | session/control_lanes.rs, session/keeper_admission.rs | – |
| session-core-reprove.ts | NONE | – | Fail-closed core rebuild |
| session-diag-snapshot.ts | PORTED | diag_snapshot.rs | – |
| session-emit.ts | PARTIAL | session/emit.rs, session/emit_streams.rs, runtime/channel_delivery.rs, runtime/cell_delivery.rs | `emit_cell_frame` never called; no query replies |
| session-git-ports.ts | PARTIAL | host/sampling.rs, host/{git_branch,ports,pr_status}.rs | HostWatchers never started |
| session-lifecycle.ts | PARTIAL | session/lifecycle.rs, session/table.rs, strays.rs | Periodic stray sweep (StrayTracker) unwired |
| session-manager.ts | PARTIAL | session/lifecycle.rs, session/lifecycle_commands.rs, session/table.rs | Terminal input/IO entry points |
| session-manager-state.ts | PORTED | session/types.rs, session/lifecycle.rs | – |
| session-raw-metadata.ts | PARTIAL | session/raw_metadata.rs | Never drained into the link |
| session-record.ts | PORTED | session/types.rs, session/history.rs, session/agent_osc.rs, session/ring.rs | – |
| session-resize-capture.ts | PORTED | runtime/channel_delivery.rs (freeze/close_capture), session/resize.rs | Unwired |
| session-respawn.ts | PORTED | session/respawn.rs | – |
| session-respawn-admission.ts | PORTED | session/respawn.rs, session/lifecycle.rs | – |
| session-resume.ts | PARTIAL | session/resume.rs, session/resume_core.rs, runtime/adoption.rs | keeper_pool/session_seam.rs `channel_history` always refuses (NO_REPORTED_HEAD / NO_REPORTED_BASE_GEOMETRY), so adoption always respawns |
| session-resume-events.ts | PORTED | session/binding.rs, session/binding_staging.rs | – |
| session-scrollback.ts | PARTIAL | session/scrollback.rs | Ordered query-reply lane |
| session-snapshot-cursor.ts | PORTED | session/snapshot_cursor.rs, session/snapshot_cursor_drain.rs | – |
| session-spawn.ts | PORTED | session/spawn.rs, keeper_pool/shell_spawner.rs, keeper_pool/pool_spawn.rs | – |
| session-sync-output.ts | PARTIAL | stream_fence.rs (SyncOutputHold) | Never constructed; the core has no synchronizedOutput |
| session-terminal-control.ts | NONE | (KeeperPool::input exists, uncalled) | Coordinator/local input and stream-state entry |
| session-terminal-metadata.ts | PARTIAL | runtime/link_loop/volatile.rs | No producer or fair flush loop |
| session-terminal-state.ts | TYPES | session/emit_streams.rs, session/cell_scheduler.rs, session/binding.rs | – |
| session-terminal-txn.ts | PARTIAL | session/resize.rs, session/control_lanes.rs (enqueue) | Coordinator stream-state transaction and ambiguous-boundary outcome; no caller |
| session-unhandled-seq.ts | PARTIAL | session/history.rs, session/scrollback.rs record_unhandled | The core cannot report sequences; no call site |

## local-door
| v2 path | status | rust path(s) | missing |
|---|---|---|---|
| local-ui-server.ts | PARTIAL | runtime/door_serve.rs (bind), door/mod.rs (paths) | No accept, HTTP/SPA, WS upgrade, or Host/Origin checks |
| local-ui-terminal-socket.ts | NONE | – | – |
| local-terminal-grants.ts | PARTIAL | local_door.rs (grant digest, TTL) | No session scope; grant frames unhandled; unwired |
| local-terminal-prehello.ts | PORTED | local_door.rs PreHelloOwner | Unwired |
| local-terminal-scrollback.ts | NONE | (shared reader: scrollback_read.rs) | Local wrapper |
| local-terminal-socket.ts | NONE | – | – |
| local-terminal-socket-authority.ts | NONE | – | – |
| local-terminal-socket-controls.ts | NONE | – | – |
| local-terminal-socket-delivery.ts | NONE | – | – |
| local-terminal-socket-input.ts | NONE | – | – |

## keeper
| v2 path | status | rust path(s) | missing |
|---|---|---|---|
| multiplexed-main.ts | PORTED | keeper:bin/roost-keeper.rs, keeper:server.rs, keeper:keeper.rs | – |
| keeper-frame-handler.ts | PORTED | keeper:keeper.rs, keeper:keeper_ops.rs | – |
| keeper-history.ts | PORTED | keeper:channel_history.rs, keeper:history.rs | – |
| keeper-input-queue.ts | PARTIAL | keeper:keeper_ops.rs, keeper:pty_channel.rs (WriteOutcome) | No per-channel FIFO, byte/command budgets or write deadline [INFERENCE] |
| keeper-log.ts | PARTIAL | tracing in keeper | Open-fd count diagnostic |
| keeper-process-reap.ts | PARTIAL | keeper:pty_channel.rs:306 kill, keeper:keeper.rs reap_exited | No process-group or tree SIGTERM/SIGKILL sweep |
| keeper-resize-result.ts | PORTED | keeper:keeper_ops.rs resize_status, keeper:client_resize.rs | – |
| keeper-stamp.ts | PORTED | keeper:keeper.rs contract()/implementation_digest, proto:keeper_update/contract.rs | – |
| keeper-types.ts | TYPES | keeper:keeper.rs, keeper:pty_channel.rs | – |
| keeper-contract.generated.ts | GENERATED | keeper:keeper.rs implementation_digest (computed at runtime) | – |
| keeper-probe.ts | PARTIAL | runtime/keeper_probe.rs, keeper:client_connect.rs | Administrative shutdown (keeper_boot.rs:228 bails) |
| keeper-pool-channels.ts | PARTIAL | keeper_pool/pool_spawn.rs, keeper_pool/channels.rs, keeper_pool/session_seam.rs | `channel_history` refuses |
| keeper-pool-config.ts | PORTED | keeper:client_connect.rs, hostc:paths.rs, runtime/keeper_boot.rs | – |
| keeper-pool-io.ts | PORTED | keeper_pool/pool.rs (input/kill), keeper_pool/session_seam.rs (resize) | Uncalled for input/resize |
| keeper-pool-lifecycle.ts | PORTED | keeper_pool/pool.rs, keeper_pool/dispatch.rs, keeper:client_connect.rs | – |
| multiplexed-client.ts | PORTED | keeper_pool/mod.rs, keeper_pool/pool.rs, runtime/keeper_boot.rs | – |
| protocol.ts | TYPES | keeper:codec.rs (barrel) | – |
| protocol-envelope.ts | PORTED | keeper:codec.rs, keeper:frames.rs | – |
| protocol-io.ts | PORTED | keeper:payloads.rs, keeper:client_connect.rs | – |
| protocol-terminal.ts | PORTED | keeper:client_resize.rs, keeper:history.rs, keeper:payloads.rs | – |
| histfile.ts | PORTED | host/shell_bootstrap.rs | PowerShell dropped |
| update-admission.ts | NONE | contract types: proto:keeper_update/contract.rs | Update action executor |

## host
| v2 path | status | rust path(s) | missing |
|---|---|---|---|
| config.ts | PORTED | runtime/boot.rs, hostc:env.rs, hostc:paths.rs | – |
| git-branch.ts | PORTED | host/git_branch.rs (polled, via sampling.rs) | Watcher unwired |
| host-identity.ts | PORTED | host/identity.rs | Windows |
| host-sample-darwin.ts | PORTED | host/samples.rs | Unwired |
| host-sample-linux.ts | PORTED | host/samples.rs (incl. cgroup pressure) | Unwired |
| host-sample-types.ts | TYPES | host/mod.rs HostSample/NetCounters | – |
| host-sample-win32.ts | NONE | – | Windows not ported |
| install.ts | PORTED | runtime/bootstrap_redeem/{mod,activation,register,label}.rs | – |
| jwt.ts | PORTED | host/jwt.rs, host/openssh_key.rs, runtime/credential.rs | – |
| listening-ports.ts | PORTED | host/ports.rs | – |
| pr-status.ts | PORTED | host/pr_status.rs, host/tool_path.rs | – |
| service-definition-env.ts | PORTED | host/install.rs | – |

## diag
| v2 path | status | rust path(s) | missing |
|---|---|---|---|
| byte-capture.ts | PORTED | capture/byte_window.rs (fed by session/emit_ingest.rs via CaptureTap::retain_output) | – |
| capture-storage.ts | PORTED | capture/storage.rs (gzip `terminal-incident-<uuid>.json.gz` in the log dir, combined retention) | – |
| terminal-capture-ack.ts | PORTED | capture/ack.rs | – |
| terminal-capture.ts | PORTED | capture/{recorder,tap}.rs, browser_commands/diagnostics.rs; taps in session/{emit_frame,emit_ingest,resize,core_reprove}.rs | – |
| terminal-capture-registry.ts | PORTED | capture/registry.rs | – |
| terminal-capture-bundle-writer.ts | PORTED | capture/bundle_writer.rs (+ proto terminal_capture/validate*.rs gate) | – |
| terminal-capture-evidence.ts | PORTED | capture/evidence.rs (+ proto terminal_capture/envelope.rs) | – |
| terminal-capture-write.ts | PORTED | capture/{write,finish}.rs | – |
| terminal-capture-emission.ts | PORTED | capture/emission.rs (+ proto terminal_capture/view.rs) | – |
| terminal-capture-pools.ts | PORTED | capture/pools.rs | – |
| terminal-capture-recorder.ts | PORTED | capture/recorder_state.rs | – |
| terminal-capture-worker-section.ts | PORTED | capture/{worker_section,section_grid,section_coverage}.rs | – |

## browser-commands
| v2 path | status | rust path(s) | missing |
|---|---|---|---|
| browser-command-handler.ts | PORTED | browser_commands/mod.rs, browser_commands/replies.rs, runtime/link_loop/browser.rs | – |
| browser-command-spawn.ts | PORTED | browser_commands/session_lifecycle.rs, session/lifecycle_commands.rs | – |
| browser-command-terminal.ts | PORTED | scrollback_read.rs, browser_commands/scrollback_page.rs, session/retained_grid.rs | – |
| browser-command-terminal-capture.ts | PORTED | browser_commands/diagnostics.rs | – |
| browser-command-diag.ts | PORTED | browser_commands/diagnostics.rs, diag_snapshot.rs | – |
| browser-command-files.ts | PORTED | browser_commands/file_commands.rs | – |

## boot
| v2 path | status | rust path(s) | missing |
|---|---|---|---|
| boot-keeper.ts | PARTIAL | boot_keeper.rs, runtime/keeper_boot.rs, runtime/keeper_probe.rs | Force-live retire bails (no shutdown frame) |
| boot-local-terminal.ts | NONE | (door bind only) | View, grant, route and peer owners |
| boot-reconcile.ts | PARTIAL | runtime/reconcile.rs, runtime/boot_sequence.rs | Serialized keeper-death reconcile and degraded remediation |
| boot-session-reconcile.ts | PARTIAL | runtime/adoption.rs, runtime/adoption_claim.rs, runtime/reconcile.rs | Agent conversation restore; adoption refuses |
| worker-boot-admission.ts | PORTED | runtime/boot_order.rs | – |

## attachments
| v2 path | status | rust path(s) | missing |
|---|---|---|---|
| browser-command-attachments.ts | PORTED | browser_commands/attachments.rs | – |
| attachment-transfer-admission.ts | PARTIAL | attachment_transfer.rs Transfers::admit/revoke_grant | No hello/grant tuple binding; unwired |
| attachment-transfer-lease.ts | PARTIAL | attachment_transfer.rs ACTIVE_LEASE/expire_leases | No idle deadline; unwired |
| attachment-reaper.ts | PARTIAL | browser_commands/attachments.rs (MANIFEST_NAME, dir guard) | 24h/1GB sweep |
| attachment-file-store.ts | PARTIAL | browser_commands/attachments.rs manifest_lookup | Final naming, short paths, manifest write |
| attachment-operation-receipts.ts | NONE | attachments/mod.rs Carrier/OperationDescriptor (types) | – |
| attachment-operation-journal.ts | NONE | – | – |
| attachment-operation-owner.ts | NONE | – | – |
| attachment-upload.ts | NONE | – | – |
| attachment-file-hash.ts | NONE | – | – |
| attachment-grants.ts | NONE | – | – |
| attachment-transfer-port.ts | NONE | – | – |
| attachment-direct-session.ts | NONE | – | – |
| attachment-direct-socket.ts | NONE | – | – |
| attachment-direct-frames.ts | NONE | – | – |
| attachment-peer-connection.ts | NONE | – | – |
| attachment-peer-owner.ts | NONE | – | – |
| attachment-peer-packet-budget.ts | NONE | – | – |
| attachment-peer-packet-port.ts | NONE | – | – |
| attachment-peer-request-validation.ts | NONE | – | – |
| local-ui-attachment-socket.ts | NONE | door/mod.rs constants only | – |

## agents
| v2 path | status | rust path(s) | missing |
|---|---|---|---|
| occupancy.ts | PORTED | agent_occupancy.rs | Test-only callers |
| registry.ts | PARTIAL | agent_occupancy.rs (observe/lose/acknowledge) | Lease, publish and proof |
| report-protocol.ts | PARTIAL | agents/mod.rs AGENT_REPORT_MAX_LINE_BYTES | Request schemas |
| process-scan.ts | PARTIAL | agents/mod.rs BuiltinAgentId | Scan |
| report-server.ts | NONE | – | – |
| report-transport.ts | NONE | – | – |
| stable-detection.ts | NONE | – | – |
| standalone-integration.ts | NONE | – | – |
| process-tree.ts | NONE | – | – |
| reference-admission.ts | NONE | proto:agent_conversation_reference.rs (types) | – |
| manifest-engine.ts | NONE | – | – |
| manifests.ts | NONE | – | – |
| peer-process-id.ts | NONE | – | – |
| detector.ts | NONE | – | – |
| environment.ts | NONE | – | – |
| install-integrations.ts | NONE | – | – |
| integration-assets.ts | NONE | – | – |
| integration-assets.generated.ts | NONE | – | – |
| integration-install-proof.ts | NONE | – | – |
| integration-install-transaction.ts | NONE | – | – |
| agent-conversation-restore.ts | NONE | – | – |
| agent-prompt-control.ts | NONE | proto:terminal_input.rs (caps, build_pty_payload) | – |
| agent-prompt-submit.ts | NONE | – | – |
| integrations/omp/roost-agent-reference.ts | NONE | – | – |
| integrations/omp/roost-agent-state.ts | NONE | – | – |
| integrations/pi/roost-agent-state.ts | NONE | – | – |

## Counts
| status | count |
|---|---|
| PORTED | 73 |
| PARTIAL | 43 |
| NONE | 80 |
| TYPES/GENERATED | 10 |
| total | 206 |

## (1) Where input and resize reach the keeper

**Input.** The only functions that write input bytes to a channel through the keeper pool are:
- `keeper_pool/pool.rs:132` `KeeperPool::input(channel_id, bytes)`, which calls `client.write_input`.
- `keeper_pool/pool.rs:138` `KeeperPool::input_sequenced(channel_id, input_seq, bytes)`, which calls `client.write_input_sequenced`.

Those delegate to `keeper:client.rs:209` and `:216` (PtyIn frame). On the keeper side, `keeper:keeper_ops.rs:137` and `:155` call `pty_channel.rs:249 write_input`.

Nothing calls either pool function in production. The only callers are tests: tests/keeper_pool_channels.rs:68,131 and tests/keeper_pool_spawn.rs:279,346. `runtime/link_drain.rs:42 on_frame` sends InputRequest and Binary to the `other =>` warn arm (~:137). `runtime/capabilities.rs:20-21` says "Nothing here routes input yet". There is no local-door socket. So browser→PTY input is not written anywhere today.

**Resize.** Public API of `session/resize.rs`:
- `PinInputs` (:33)
- `ResizeOutcome{Applied,Unchanged,Refused}` (:56)
- `pin_for(PinInputs)->SbOriginPin` (:79). Called internally at :263 and by tests.
- `pin_for_adoption(..)` (:120). Called by session/resume_core.rs:92 (production, adoption path).
- `SessionManager::resize_channel(ChannelId, cols, rows)->Result<ResizeOutcome,Refusal>` (:152). It freezes the capture, then calls `self.keeper.resize_channel(raw, seq, cols, rows)` at :184, then closes the capture, resizes the core and writes the pin.
- `SessionManager::note_applied_resize_seq` (:314). No caller at all.

The keeper seam is `session/keeper_channels.rs:46` (trait), implemented in `keeper_pool/session_seam.rs:185`.

There is no production caller of `SessionManager::resize_channel`. The only callers are tests/session_resize.rs:84,126; tests/keeper_survivor_adoption.rs calls the trait directly. TerminalStreamState frames are dropped in link_drain.

## (2) SyncOutputHold and stream fence

Both live in `stream_fence.rs`:
- `SYNC_OUTPUT_MAX_SILENT`, `SYNC_OUTPUT_MAX_PENDING_ROWS`, `Generation`, `Scheduled`, `Fence`, `HoldAction`
- `SyncOutputHold` (:178), with `new`, `is_open`, `open`, `close`, `note_pending`, `action`, and `Default`

Neither is constructed anywhere in `src/` outside this file. lib.rs:40 only declares the module, and term:lib.rs:17 mentions it in a comment. The only uses are in tests/stream_fence.rs. `session/cell_scheduler.rs:39` has a `CellGate::SyncOutput` label, but nothing opens that gate from a hold.

## (3) Public APIs and production callers

**local_door.rs** — no production caller for any item (no `local_door::` import in src or runtime).
- `PREHELLO_DEADLINE` (3s)
- `MAX_ESTABLISHED` (32)
- `type SocketId`
- `struct Authenticated{socket_id, replaced}`
- `enum Refusal{AlreadyAdmitted, AtCapacity, UnknownGrant, BadSecret}`
- `enum Authentication{Authenticated, Refused}`
- `struct PreHelloOwner`, with `new`, `next_socket_id`, `admit`, `established`, `expire`, `install_grant`, `authenticate`, `grant_for`, `close`, `expire_grants`
- `fn sha256_of`

**door/mod.rs** — no production caller (hits for these names elsewhere are roost-client-core's own copies).
- `LOCAL_TERMINAL_SUBPROTOCOL`
- `LOCAL_TERMINAL_PATH`
- `LOCAL_BOOTSTRAP_PATH`
- `LOCAL_ATTACHMENT_PATH`
- `LOCAL_ATTACHMENT_SUBPROTOCOL`
- `LOCAL_TERMINAL_MAX_PAYLOAD_BYTES`
- `LOCAL_TERMINAL_MAX_BACKPRESSURE_BYTES`

**runtime/door_serve.rs**
- `ENV_DOOR_BIND`: production caller boot_sequence.rs:381.
- `DEFAULT_DOOR_ORIGIN`: no caller.
- `struct LocalDoor`
- `LocalDoor::bind`: production caller boot_sequence.rs:113.
- `address()`: tests only.
- `listener()`: no caller, so nothing is ever accepted.
- `origin()`: production caller boot_sequence.rs:356 (log line).
- `fn web_dist_path`: no caller.

**peer/mod.rs** — no production caller.
- `enum OfferFault{InvalidSdp, MissingGrant, ExpiredGrant, IdentityMismatch}`, with `as_str` and `parse`

**agents/mod.rs** — no production caller.
- `enum BuiltinAgentId` (10 variants), with `ALL`, `as_str`, `parse`
- `AGENT_REPORT_MAX_LINE_BYTES` (32 KiB)

**attachments/mod.rs** — no production caller. The `attachments::` hits in src are `browser_commands::attachments`, a different module.
- `enum Carrier{Coordinator, Direct}`, with `as_str`
- `struct OperationDescriptor{request_id, session_id, filename, short_path, total_bytes}`

**capture/mod.rs**
- `pub mod ack`, `bundle_writer`, `byte_window`, `emission`, `evidence`, `finish`, `pools`, `recorder`, `recorder_state`, `registry`, `section_coverage`, `section_grid`, `storage`, `tap`, `worker_section`, `write`
- `CaptureRecorder::attach_to_emitter`: production caller runtime/session_stack.rs (one recorder; `deps` hands it to diagnostics; session-closed hook drops its state; owners shutdown stops the retention sweep).
- `BYTE_CAPTURE_WINDOW_BYTES` (256 KiB): used by byte_window.rs.

**agent_occupancy.rs** — no production caller; only tests/agent_occupancy.rs and tests/agent_screen_signal.rs.
- `enum Source`
- `enum RuntimeState`, with `forced_idle`
- `struct Candidate`
- `struct ProcessKey`, with `new`
- `struct Occupant`, with `awaits_acknowledgement`
- `enum Loss`
- `struct Occupancy`, with `new`, `occupants`, `get`, `observe`, `lose`, `acknowledge`, `awaiting_acknowledgement`

**strays.rs**
- `SWEEP_INTERVAL`, `STRAY_STRIKES`, `DEGRADED_THRESHOLD`: no caller.
- `RECENTLY_CLOSED_TTL`: production, session/lifecycle.rs:31.
- `DEAD_BIRTH_LIFETIME`, `DEAD_BIRTH_THRESHOLD`, `DEGRADED_WINDOW`: production, session/respawn.rs:18.
- `enum Verdict{Keep, Strike, Reap}`: tests only.
- `struct StrayTracker`, with `new`, `on_spawn`, `on_session_closed`, `sweep`, `strikes`: tests only.
- `struct Birth`, with `new`, `produced`, `verdict`: tests only.
- `enum Stillborn`, with `is_stillborn`: production, respawn.rs:18.
- `struct ChannelAllocator`, with `new`, `next`, `advance_past_keeper`, `take`, `is_exhausted`: production, session/lifecycle.rs:83,131.

**host/samples.rs** — no production caller for any item.
- `struct CgroupPressure`
- `struct HostSampler`, with `new` and `sample`
- `fn sample_host`
- `sample_linux_memory`
- `sample_disk`
- `sample_linux_net`
- `sample_darwin_net`
- `sample_cgroup_pressure`

**host/sampling.rs** — no production caller; only used inside this file.
- `HEAD_POLL_INTERVAL`, `FACTS_POLL_INTERVAL`
- `struct FolderFacts`
- `type FactsSink`
- `struct HostWatchers`, with `new`, `watch`, `stop`, `stop_all`, `is_watching`, `watched`, `live_watchers`
- `fn read_folder_facts`

## Wiring gaps that shaped the statuses
- `session/emit.rs:175 emit_cell_frame` has no caller anywhere, and nothing in runtime drains raw metadata. The ingest path (runtime/channel_delivery.rs:111 into emit.rs:133) only marks channels dirty.
- `keeper_pool/session_seam.rs` `channel_history` always refuses, so survivor adoption always ends in a respawn.
- `runtime/keeper_boot.rs:228`: force-live retire bails because there is no keeper shutdown frame.
- `term:lib.rs:9-27`: the core has no getResponse/writeRaw (the query-reply lane needs these) and no synchronizedOutput (`SyncOutputHold` is written against it).