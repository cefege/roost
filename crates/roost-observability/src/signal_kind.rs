//! The Tier-1 anomaly vocabulary. This enum is documentation as much as code:
//! each variant's doc comment is the operator-facing statement of what that
//! signal means, and the literal beside it is the token that lands in the log
//! line. The spelling is a contract — `roost doctor` counts these tokens and
//! an operator greps them — so it never changes. Tier-1 anomalies are
//! low-volume, always on and cooldown-gated; context-level detail stays on
//! [`crate::diag::emit`].

/// Declares the enum, the exhaustive list and the literal table from one
/// list, because three hand-kept lists of a closed vocabulary drift: a new
/// kind added to the enum and forgotten in `as_str` compiles and then loses
/// the token at runtime.
macro_rules! signal_kinds {
    ($($(#[$meta:meta])* $variant:ident => $literal:literal,)*) => {
        /// One genuine anomaly, always on and cooldown-gated. Deliberately
        /// out of `diag`: these are the lines a daily review reads, so keep
        /// callsites rare or lean on the per-kind cooldown.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum SignalKind {
            $(
                $(#[$meta])*
                $variant,
            )*
        }

        impl SignalKind {
            /// Every kind, in the order the vocabulary grew.
            pub const ALL: [SignalKind; 62] = [$(Self::$variant,)*];

            /// The token that goes into the log line. Exact.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $literal,)*
                }
            }

            /// Read a kind back off a log line. An unrecognized token is
            /// `None` rather than a guess: a newer peer may emit a kind this
            /// build never heard of, and misreading it would file it under
            /// the wrong anomaly.
            pub fn from_name(name: &str) -> Option<Self> {
                Self::ALL.into_iter().find(|kind| kind.as_str() == name)
            }
        }

        impl std::fmt::Display for SignalKind {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

signal_kinds! {
    /// the browser's wasm panic hook or an unhandled error reached the top of the SPA
    SpaUncaught => "spa.uncaught",
    /// SPA anomaly detectors (kv.kind = the detector)
    DiagCorruptionSignal => "diag.corruption_signal",
    /// duplicated browser tab inherited sessionStorage identity; newcomer rotated before opening
    /// authenticated transports
    TabDuplicateIdentityRotated => "tab.duplicate_identity_rotated",
    /// the device key had been minted before but its slot was empty at boot, so a fresh key
    /// replaced it and the device must pair again
    AuthKeyEvicted => "auth.key_evicted",
    /// the coordinator rejected this browser's device credential with a 401 and it must pair again
    AuthRelogin401 => "auth.relogin_401",
    /// the composer or a terminal refused a burst of keystrokes, once per minute per pane at most
    InputDropBurst => "input.drop_burst",
    /// keeper subprocess exited → all PTYs lost; worker respawns + reconciles sessions
    KeeperDied => "keeper.died",
    /// keeper alive but births dead PTYs (spawn ok, no I/O) — emit_no_session burst; fix = fresh
    /// keeper
    KeeperDegraded => "keeper.degraded",
    /// restart budget exhausted (≥N restarts/window) — restarting again just re-SIGTERMs live
    /// PTYs; STOP + alert
    KeeperDegradedUnrecoverable => "keeper.degraded_unrecoverable",
    /// a single spawn → instant zero-byte exit (head_seq===0, <2s); N within window →
    /// keeper.degraded self-heal
    KeeperDeadBirth => "keeper.dead_birth",
    /// worker force-restarted a degraded survivor keeper (self-heal; grace-gated to avoid loops)
    KeeperRestartDegraded => "keeper.restart_degraded",
    /// keeper never acked a Spawn frame within timeout → session hangs (degraded/wedged keeper)
    SpawnNoAck => "spawn.no_ack",
    /// ring rolled past lastSeq → silent history hole on resume (observability, NOT a band-aid
    /// trigger)
    ScrollbackGap => "scrollback.gap",
    /// a core rebuild could not reproduce the history the core it replaced still held, and/or its
    /// monotonic origin pin CLAMPED — the "history shrank / mis-spliced after a resize" class. A
    /// rebuild replays a FIXED byte ring, so once that ring no longer reaches as far back as the
    /// old core's line ring the history floor silently JUMPS; kv names the pin's before/after
    /// values, how many rows the replay could not reach, and whether the clamp fired. The one
    /// moment sbOrigin's correctness is in doubt, so it reports even though the rebuild itself
    /// succeeded
    ScrollbackReplayBound => "scrollback.replay_bound",
    /// worker cell-emission gate outlived the keeper command budget: a resize transaction (or
    /// repair) is stalling frames, so kv names the gate, its monotonic age, the transaction phase,
    /// and the captured byte count
    TerminalGateOverBudget => "terminal.gate_over_budget",
    /// in-place core resize/recovery trapped; the stream is fail-closed and later PTY bytes stay
    /// in ordered recovery records until adoption
    TerminalCoreFailed => "terminal.core_failed",
    /// worker rebuilt a frozen terminal core in place from the keeper's ordered history; the
    /// stream can emit again and its grid identity is NEW, so browsers re-derive absolute rows
    /// instead of merging into retained ones
    TerminalCoreReproved => "terminal.core_reproved",
    /// worker refused a new/adopted terminal core before partial construction; kv reports bounded
    /// admission counters
    TerminalCoreCapacity => "terminal.core_capacity",
    /// worker canonical full exceeded structural/chunk limits and cannot establish a baseline for
    /// the stream
    TerminalInvalidFrame => "terminal.invalid_frame",
    /// coordinator replica bounds rejected a newly completing screen without evicting an active
    /// session
    TerminalScreenCapacity => "terminal.screen_capacity",
    /// coordinator could not encode a canonical cached snapshot for incremental recipient delivery
    TerminalSnapshotEncodeFailed => "terminal.snapshot_encode_failed",
    /// an application opened a DEC 2026 synchronized-output frame and did not close it inside
    /// either ceiling, so the worker force-emitted the withheld cell frame and stopped suppressing
    /// that generation; kv names which cap tripped, the generation, the hold's monotonic age and
    /// how many frames it withheld
    TerminalSyncOutputCap => "terminal.sync_output_cap",
    /// the terminal core's dispatcher IGNORED an escape sequence the application sent — the
    /// "renders wrong in Roost, fine in iTerm" class (e.g. DA1 `CSI c`, DECSCUSR `CSI Ps SP q`);
    /// kv names final/private/param_count/params plus ring_full+dropped. ONE line per distinct
    /// sequence per core instance (a rebuild reports afresh), per-channel cooldown. Partial
    /// detector: the core never logs unhandled OSC (other than 0/2/8) or unimplemented
    /// DECSET/DECRST modes
    TerminalUnhandledSequence => "terminal.unhandled_sequence",
    /// an operator armed opt-in terminal incident recording on one session; kv names recording ID,
    /// lease expiry and which layers acknowledged. Consent for sensitive capture was granted HERE
    /// — the audit trail for why raw terminal bytes exist on disk at all
    TerminalCaptureStarted => "terminal.capture_started",
    /// one incident bundle was frozen and written; kv names capture ID, byte length, per-layer
    /// coverage and the trigger reason. cooldownKey is the capture ID so a distinct saved file is
    /// never coalesced away by the generic per-kind cooldown
    TerminalCaptureSaved => "terminal.capture_saved",
    /// a capture could not complete; kv names the fixed error code and which layers were
    /// unavailable. The browser still holds its frozen evidence for retry, so this is not evidence
    /// loss
    TerminalCaptureFailed => "terminal.capture_failed",
    /// recording released by the owner; in-memory trace state freed, saved files retained
    TerminalCaptureStopped => "terminal.capture_stopped",
    /// a lease passed its server-time expiry without renewal; recorders disarmed themselves and a
    /// page reload must START again rather than silently resume
    TerminalCaptureExpired => "terminal.capture_expired",
    /// a fresh viewport-only core scan disagreed with the worker's own emitted-frame fold at one
    /// emission's exact generation and sequence — the grid the core holds is not the grid the
    /// worker shipped. kv names the differing field plus row/column, never the text
    TerminalEmissionConflict => "terminal.emission_conflict",
    /// the dictation WebSocket closed without the user ending the recording; kv names the stage,
    /// close code and reason
    VoiceWsFailed => "voice.ws_failed",
    /// mic capture never started (getUserMedia denied/busy/absent, or the pipeline was torn down
    /// mid-start) — the failure class the Deepgram WS signal is blind to
    VoiceMicFailed => "voice.mic_failed",
    /// a recording ended with an EMPTY transcript and no error anywhere; kv names the stage that
    /// went quiet (frames=0 dead audio graph / peak=0 device silence / chunks>0 results=0 Deepgram
    /// never answered / results>0 chars=0 it heard nothing) plus build+ua, because "the mic does
    /// nothing" is otherwise unfalsifiable after the tab closes
    VoiceDictationEmpty => "voice.dictation_empty",
    /// MainPane dead-route safety net navigated away from a terminal route
    /// (kv.reason=gone|stale-deeplink, kv.target). Live-session bounce = a resolution bug to chase
    /// from kv.sid.
    NavSafetyNetRedirect => "nav.safety_net_redirect",
    /// SPA main-thread task ≥ freeze threshold; kv carries the leak-watch accumulator snapshot
    /// (per-session map sizes, dom_nodes, heap_mb, uptime) at stall time → names days-long-uptime
    /// bloat vs a transient
    PerfLongtaskStall => "perf.longtask_stall",
    /// Cloudflare Access assertion was absent or failed verification
    CfAccessRejected => "cf-access.rejected",
    /// coord byte-hub dropped PTY output/cell/status for a channel with no session mapping (burst
    /// = real output/history loss, not the open-race)
    BytesDropUnmapped => "bytes.drop_unmapped",
    /// coord's announced-channel barrier abandoned buffered worker frames; a mapped terminal
    /// stream is invalidated and requests one full snapshot
    CellAnnounceBarrierDrop => "cell.announce_barrier_drop",
    /// coord reconnect backfill query threw; live stream continued → SPA split-brain
    SyncBackfillFailed => "sync.backfill_failed",
    /// coord backfill hit the getEventsSince row cap → events silently skipped
    SyncBackfillTruncated => "sync.backfill_truncated",
    /// coord Sync per-stream queue crossed high-water → slow subscriber / runaway producer
    SyncQueueOverflow => "sync.queue_overflow",
    /// coord rejected a browser Sync WS upgrade (jwt invalid / missing token); kv.reason
    SyncAuthRejected => "sync.auth_rejected",
    /// coord's ws.send returned 0 = the frame was DROPPED, not merely backpressured. A cell frame
    /// lost here is what the SPA's cell.seq_gap then recovers from; without this the coord side of
    /// that story is invisible
    SyncWsFrameDropped => "sync.ws_frame_dropped",
    /// SPA domain snapshot RPC passed its deadline and was cancelled; the domain stayed un-ready,
    /// which mutes a mounted terminal until the retry or redial lands
    SyncHydrationTimeout => "sync.hydration_timeout",
    /// SPA saw a cell-frame seq discontinuity (frame lost in transit) → forced a catch-up claim. A
    /// BURST means the socket is losing frames, not that recovery is broken
    CellSeqGap => "cell.seq_gap",
    /// foreground terminal liveness bound fired; kv.layer=view_ack|terminal_proof|dom_reconcile
    /// and kv.action=resync|redial|reconcile name the failed proof and recovery
    CellForegroundStall => "cell.foreground_stall",
    /// coord appendEvent DB tx failed (event-log durability)
    EventAppendFailed => "event.append_failed",
    /// coord audit_log insert failed (audit/compliance trail hole)
    AuditWriteFailed => "audit.write_failed",
    /// coord terminal-input audit queue reached its bounded capacity; producers are waiting for
    /// durable audit writes
    AuditInputQueueBackpressure => "audit.input_queue_backpressure",
    /// coord rejected a worker WS upgrade (jwt invalid / fp mismatch); kv.reason
    WorkerAuthRejected => "worker.auth_rejected",
    /// worker sent event-before-hello / an undecodable frame; kv.reason
    WorkerProtocolViolation => "worker.protocol_violation",
    /// coord bounded worker-frame queue rejected a frame; socket closes with 1009 before retaining
    /// it
    WorkerQueueOverflow => "worker.queue_overflow",
    /// coord worker durable-event window exceeded 600/minute; socket closes before persistence
    WorkerEventRateExceeded => "worker.event_rate_exceeded",
    /// coord→worker pending RPC timed out; browser spawn/attach hangs
    RpcWorkerTimeout => "rpc.worker_timeout",
    /// a Connect RPC returned 401/Unauthenticated (jwt verify fail or no caller); kv.reason,path
    AuthRpcRejected => "auth.rpc_rejected",
    /// worker uncaughtException/unhandledRejection (mirror of spa.uncaught);
    /// kv.kind=error|rejection
    WorkerUncaught => "worker.uncaught",
    /// replaceable cwd/git/pr/ports event was superseded or rejected within its bounded volatile
    /// lane; lifecycle durability is unaffected
    TransportMetadataCoalesced => "transport.metadata_coalesced",
    /// worker snapshot exceeded membership/byte limits or could not be encoded; the authenticated
    /// worker stays locally unready until a valid exact snapshot can cross the barrier
    TransportSnapshotUnready => "transport.snapshot_unready",
    /// N consecutive heartbeat failures to coord (worker invisible to fleet)
    HeartbeatStalled => "heartbeat.stalled",
    /// resume fell back to an empty ring after getHistory failed (full scrollback wipe)
    ScrollbackHistoryLost => "scrollback.history_lost",
    /// detached `roost deploy <host>` timed out / exited non-zero / failed to spawn
    DeployFailed => "deploy.failed",
    /// deploy continued on plain-ws after `tailscale cert` failed (worker without TLS)
    DeployCertSkipped => "deploy.cert_skipped",
    /// SPA/worker failed to sign the coordinator JWT (RPCs go out unauthenticated)
    AuthJwtSignFail => "auth.jwt_sign_fail",
}
