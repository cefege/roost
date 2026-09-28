# W-HEART phase-2 handoff (self-sufficient for a fresh agent)

Slice W-HEART of Stage 2W (plan: docs/v3-handoff/roost-v3-finish-and-cutover-plan.md "### Stage 2W").
Worktree /home/almalinux/repos/roost-v3-worker, crate crates/roost-worker unless named. Rules of the
lead's brief apply (≤400 lines/file, `//!` 3–6 line header naming v2 files, no unwrap/expect outside
tests, tracing per transition, /tmp/wcheck.sh + /tmp/wcargo.sh only, never commit/fmt, mutations
seen to fail). Drafts live here mirroring repo paths: target-track/drafts/WHeart/crates/roost-worker/…
BASE.md5 = md5 of every existing file a draft REPLACES, taken at draft time: if `md5sum` differs now,
re-apply that draft's hunks (described below) by hand instead of copying.

## What v2 does (the parity target — v2 wins over the task text)
- v2 heartbeat (apps/worker/src/transport/heartbeat.ts) sends ONLY: host_metrics (60 s cached
  resample, last good kept on failure), git_sha (omit when build sha is "dev"), os, host_identity,
  keeper_runtime (only after a reconciliation, dropped if the reconciliation changed mid-beat),
  reachable_addr (tailnet MagicDNS, 5 min cache, env ROOST_REACHABLE_ADDR fallback), terminal_core_capacity.
  Interval 30 s from settlement, RPC timeout 10 s, `heartbeat.stalled` signal at 3 consecutive misses
  (cooldownKey "heartbeat"). Git branch / ports / PR are NOT heartbeat fields: they are `git`/`pr`/`ports`
  SessionEvents from session-git-ports.ts (branch+remote at start, HEAD watch, 90 s PR poll, 90 s ports poll).
- Strays: session-lifecycle.ts reapStrayKeeperChannels (two consecutive sightings, strike deleted on
  reap, list failure → 0) + 60 s timer from startPostAdmissionMaintenance; called inside
  boot-session-reconcile.ts (:175 timer start before resumes, :316 reap at end, then reconciledAt).
- Channel-creation gate: session-channel-creation-gate.ts + session-manager.ts:44-74 (lease per
  spawnShell/respawn; beginPreparation closes admission synchronously then awaits leases; rollback
  idempotent, never automatic; keeperUpdatePrepared = gate.blocksTerminalWrites()).

## Draft files (copy into the tree)
Replacements of existing files (W-HEART owns them; check BASE.md5 first):
| draft | purpose / v2 ported |
|---|---|
| src/host/tool_path.rs | + `process_tool_path(platform)`, `run_on_path(path,program,args,cwd)`, `run_bounded(program,args,cwd,path:Option<&str>,timeout)`; stdout drained on a thread (pipe-full hang fix). `run` keeps its signature. |
| src/host/samples.rs | v2 host-sample-linux.ts. Linux half only; darwin moved to samples_darwin.rs. Fixes: /proc/net/dev searched on every line (old code only looked at the first colon line → no bandwidth ever), `ip`/`df` bare with v2's 1 s bound, CPU delta signed, cgroup probe via roost_host::host_memory::{cgroup2_base, read_linux_memory_file}. Keeps pub `sample_disk`, `sample_linux_memory`, `sample_linux_net`, `sample_cgroup_pressure`, `HostSampler`, `CgroupPressure`, `sample_host`; new pub(super) `SAMPLER_TOOL_TIMEOUT`, `read_disk`. |
| src/host/sampling.rs | HostWatchers reworked to v2's schedule; new `FolderReading` enum + `FolderFactsSink` trait (`apply(reading)->bool`, `pull_request_branch()->Option<String>`); `watch(session_id, folder, root_pid, platform, sink: Arc<dyn FolderFactsSink>)`; `stop` flags + detaches (no join — a thread in `gh` may take 10 s and close paths are async). Removed `FolderFacts`, `FactsSink`, `read_folder_facts` (no other users). |
| src/host/git_branch.rs | `GitReader::system()` (bare `git`, process PATH as v2) replaces broken `from_tool_path` (it set the PROGRAM to a PATH string); `github_owner_repo` follows v2's regex `/github\.com[:/]([^/]+)\/([^/]+?)(?:\.git)?\/?$/`. |
| src/host/pr_status.rs | `PrReader { program, path: Option<String> }`, `PrReader::on_path(path)` (runs `gh` with PATH=tool path, v2 GH_PATH) replaces broken `from_tool_path`; `RollupEntry.conclusion: Option<String>`; `rollup_checks` uses v2 `conclusion ?? state`. |
| src/host/ports.rs | ps/ss/lsof via `run_on_path(process_tool_path)` (old code hardcoded /usr/sbin/ss — absent on Debian/Ubuntu); `descendant_pids(root, tool_path)`; ss rows with `cut <= 0` skipped (v2). |
| src/strays.rs | StrayTracker = v2 strayStrikes: `sweep(&mut self, keeper_channels:&[u16], tracked:&HashSet<u16>) -> Vec<Verdict>`; removed `closed_at` TTL exemption, `on_spawn`, `on_session_closed`, `last_sweep` (not v2, no production caller); strike removed on Reap. |
| tests/strays.rs | tracker tests on the new signature; the two closed_at tests deleted (behaviour not in v2); new `a_reaped_channel_still_listed_starts_its_strikes_over`. |
| tests/host_folder_facts.rs | watcher test uses a `CountingSink: FolderFactsSink` and waits (≤15 s) for `live_watchers()==0` after stop; rollup helper maps "" conclusion → None (gh omits it on commit statuses). |

New files:
| draft | purpose |
|---|---|
| src/host/samples_darwin.rs | v2 host-sample-darwin.ts: `top -l 1 -n 0` CPU (was always 0), vm_stat leading-digit parse (old parse failed on the trailing "." → mem 0), v2 failure grouping (vm_stat/memsize fail → mem+disk 0), netstat row skip; pub `top_idle_pct`, `parse_netstat_bytes`, `sample_darwin_net`; `DarwinSampler` pub(super). |
| src/host/tailnet.rs | v2 packages/host/src/tailnet.ts: `tailscale_binary_candidates(platform, &dyn EnvSource)`, `resolve_tailnet_dns_name(&[String])` (2 s bound), `parse_self_dns_name`. |
| src/session/channel_creation_gate.rs | `ChannelCreationGate` (watch-channel counts; drives `ControlLanes::set_keeper_update_prepared` on 0↔1 edges inside the lock), `CreationLease` (Drop releases), `PreparationRollback::rollback(&self)` (idempotent, not on drop), `CHANNEL_CREATION_REFUSAL`; `impl SessionManager { keeper_update_prepared(), begin_keeper_update_preparation() -> impl Future<Output=PreparationRollback>+Send+'static, pub(super) admit_channel_creation() -> Result<CreationLease, Refusal> }`. |
| src/session/folder_hooks.rs | `SessionFolderHooks` registry (mirror of W1Wire's closed_hooks.rs): `SessionFolderHook = Arc<dyn Fn(&SessionId, u16)+Send+Sync>`; `SessionManager::on_session_folder(hook)`, `notify_session_folder(&SessionId, channel_id)`. |
| src/session/git_ports.rs | v2 session-git-ports.ts: `SessionFolderFacts::attach(&SessionManager, HostPlatform, tokio Handle) -> Arc<Self>` registers folder hook (start/restart watcher) + closed hook (stop); `apply(&SessionId, channel, FolderReading)->bool` compares with the record (only if the channel's record is still that session), writes it, publishes `Git`/`Pr`/`Ports` via `events.emit(&e, None)` with `Handle::block_on` (called from the watcher thread only); `pull_request_branch`; `start`, `stop`, `stop_all`, `is_watching`. |
| src/session/stray_reap.rs | `StraySweeper::new(Arc<SessionManager>) -> Arc<Self>`; `async reap_stray_keeper_channels(&self)->usize` (keeper list on spawn_blocking, tracked = table has channel, kill_channel on spawn_blocking, logs `stray_keeper_channel_reaped`/`stray_reap_list_failed`); `start_post_admission_maintenance(self:&Arc<Self>)->bool` (idempotent, 60 s `interval_at`, Weak self); `maintenance_running`, `dispose`. |
| src/runtime/heartbeat.rs | v2 startHeartbeat: `HeartbeatSources`, `HeartbeatRpc` traits, `CapacityReader`, `KeeperReconciliation {started, reconciled(ms), current}`, `HeartbeatConfig`, `HeartbeatHandle {stop, is_finished}` (drop also stops), `start_heartbeat(config).await` (awaits first attempt, v2), `spawn_heartbeat(config)` (boot uses it: v2's link was already dialling; awaiting here would delay link.run by up to 10 s). Next sleep is created BEFORE signalling first-settled (v2 scheduleNext). Proto conversion errors count as a miss (inside v2's try). |
| src/runtime/heartbeat_metrics.rs | v2 collectHostMetrics + logCgroupPressure: `HostMetricsCollector::{for_host(platform, clock), new(sample, cgroup, clock), collect()}`, `HOST_METRICS_INTERVAL_MS`. |
| src/runtime/heartbeat_sources.rs | `WorkerHeartbeatSources::new(collector, git_sha, tailscale_candidates, Arc<KeeperPool>, clock)` (metrics + tailnet on spawn_blocking; keeper proof `pool.probe_runtime().await.ok().and_then(|p| p.observation(ms))`), `CoordinatorHeartbeatRpc::new(CoordinatorServiceClient<HttpClient>, Arc<dyn CredentialSource>)` (fresh JWT per call, v2 interceptor: mint failure → warn "jwt mint failed", send unauthenticated), `REACHABLE_ADDR_TTL_MS`. |
| src/runtime/heart_owners.rs | `HeartOwners::build(&SessionStack, Arc<KeeperPool>, HostPlatform)` (attaches folder facts; must run before survivor adoption), pub fields `strays`, `reconciliation`, `folder_facts`; `start_heartbeat(&mut self, &HeartbeatTarget{coordinator_base, worker_key_path}) -> anyhow::Result<()>`; `shutdown(self)` (heartbeat stop first, strays dispose, watchers stop_all). |
| tests/heartbeat.rs + tests/heartbeat_support/mod.rs | port of apps/worker/tests/transport/heartbeat.test.ts (8 tests, paused tokio clock). |
| tests/heartbeat_host_metrics.rs | collector cache window, bandwidth over the minute, counter reset → 0. |
| tests/channel_creation_gate.rs | port of tests/session/session-channel-creation-gate.test.ts tests 1–2 at the manager seam + rollback-idempotence gate test. |
| tests/session_git_ports.rs | pins session-git-ports.ts apply semantics + hook lifecycle (v2 has no test file for it). |
| tests/stray_reap.rs | port of tests/keeper-stray-reap.test.ts (ScriptedKeeper) + maintenance-timer pins of tests/boot/boot-reconcile-admission.test.ts. |
| tests/host_listening_ports.rs | port of tests/host/listening-ports.test.ts + portsEq cases of pr-status.test.ts. |
| tests/host_pr_status.rs | port of tests/host/pr-status.test.ts (rollup, prStatusEq) + tests/host/git-branch.test.ts (real git in repo / "/"). |
| tests/host_samples.rs | every /proc/net/dev interface yields counters; darwin top/netstat parsers. |

## Exact edits to existing files (re-read each right before editing; siblings edit concurrently)
1. src/host/mod.rs: in the `pub mod` block add `pub mod samples_darwin;` (after `samples`) and `pub mod tailnet;` (after `shell_spec_resolver`). Header line "`runtime::serve` owns the samplers" → "`runtime::heartbeat_metrics` owns the samplers".
2. src/session/mod.rs: append `pub mod channel_creation_gate;`, `pub mod folder_hooks;`, `pub mod git_ports;`, `pub mod stray_reap;`.
3. src/runtime/mod.rs: append `pub mod heartbeat;`, `pub mod heartbeat_metrics;`, `pub mod heartbeat_sources;`, `pub mod heart_owners;`.
4. src/session/lifecycle.rs (389 lines at draft time — stay ≤400): in `pub struct SessionManager` append
   ```rust
   /// v2 `#channelCreationGate`: spawn/respawn leases vs keeper-update preparation.
   pub(super) creation_gate: super::channel_creation_gate::ChannelCreationGate,
   /// Owners told when a live session's folder is (re)established.
   pub(super) folder_hooks: super::folder_hooks::SessionFolderHooks,
   ```
   In `SessionManager::new`, inside the `Arc::new_cyclic(|self_handle| …)` closure: bind
   `let lanes = Arc::new(super::control_lanes::ControlLanes::new());` before `Self {`, replace the
   `lanes: Arc::new(super::control_lanes::ControlLanes::new()),` line with
   `creation_gate: super::channel_creation_gate::ChannelCreationGate::new(Arc::clone(&lanes)), lanes,`
   and add `folder_hooks: Default::default(),`.
5. src/session/respawn.rs: first statement of `open_shell` AND of `respawn_lost_child`:
   `let _lease = self.admit_channel_creation()?;` (named binding — `let _ =` would drop it at once).
   In `open_under`: before `if let Err(error) = self.sessions.insert(record) {` add
   `let opened_session = record.session_id().clone();`; just before the final `Ok(SessionOutcome::Spawned {`
   add `self.notify_session_folder(&opened_session, raw_channel);` (v2 session-spawn.ts:151 / session-respawn.ts:181).
6. src/session/resume.rs `adopt_survivor`: before the final `Ok(Adopted { replay_offset, head_seq })`
   (after the `install_stream` + "a keeper survivor was adopted" log) add
   `self.notify_session_folder(&request.session_id, channel);` (v2 session-resume.ts:278). Not in the
   held-exit branch (that record is already closed).
7. src/session/cwd_events.rs (W1Wire's) `CwdEventWriter::run`: after the `match manager.events.emit(..)` block, inside the loop:
   ```rust
   // v2 session-scrollback.ts:187-190: a new folder may be another repo — re-watch it.
   if let Some(session_id) = event.session_id()
       && let Some(channel_id) = manager.sessions.channel_of(session_id)
   {
       manager.notify_session_folder(session_id, channel_id);
   }
   ```
8. src/runtime/owners.rs: `use super::heart_owners::HeartOwners;`; struct field `pub heart: HeartOwners,`;
   `WorkerOwners::build` gains `platform: roost_host::HostPlatform`; before `let keeper: Arc<dyn KeeperPipelineSource> = pool;`
   add `let heart = HeartOwners::build(&stack, Arc::clone(&pool), platform);`; put `heart` in `Self { .. }`;
   `shutdown(self)`: first line `self.heart.shutdown();`. HeartOwners::build must run before survivor adoption.
9. src/runtime/boot_sequence.rs: pass `platform` (already bound at step 3) to `WorkerOwners::build(..)`.
   After the `StepId::Ready` log and before `link.run(..)`:
   ```rust
   owners.heart.start_heartbeat(&super::heart_owners::HeartbeatTarget {
       coordinator_base: boot.coordinator_base.clone(),
       worker_key_path: boot.worker_key_path.clone(),
   })?;
   ```
   (adapt `owners` to W1Wire's actual binding name). Reconcile calls — see "Reconcile wiring" below.
10. src/runtime/bootstrap_redeem/mod.rs: `const AUTHORIZATION` → `pub(crate) const AUTHORIZATION`;
    `fn build_sha(` → `pub(crate) fn build_sha(`.
11. crates/roost-worker/Cargo.toml `[dev-dependencies]` tokio features: add `"test-util"` (paused clock; roost-coord already does). No Cargo.lock change. Cross-owner edit → report it.
12. tests/session_support/mod.rs (shared by several test binaries; `#![allow(dead_code)]` already there):
    add `use roost_worker::session::spawn::ShellSpawner;` and split `with_capacity`:
    ```rust
    pub fn with_capacity(keeper: Arc<ScriptedKeeper>, terminal_core_cap: Option<u32>) -> Self {
        Self::build(keeper, terminal_core_cap, Arc::new(NeverSpawns))
    }
    /// A manager whose PTY opens go to `spawner` (the creation-gate tests hold one).
    pub fn with_spawner(spawner: Arc<dyn ShellSpawner>) -> Self {
        Self::build(Arc::new(ScriptedKeeper::default()), None, spawner)
    }
    fn build(keeper: Arc<ScriptedKeeper>, terminal_core_cap: Option<u32>, spawner: Arc<dyn ShellSpawner>) -> Self {
        /* old with_capacity body, with `Arc::new(NeverSpawns)` replaced by `spawner` */
    }
    ```
    Then tell WKUpdate (agent://WorkerLead2W2.WKUpdate) it is in the tree.

## Reconcile wiring (open item — decide at wiring time)
Agreed with WResume: ITS reconcile pass (runtime/session_reconcile.rs + reconcile_gate.rs, replacing the
boot adoption step, also run on keeper death) calls, from `owners.heart`:
`reconciliation.started()` at runReconcile start; `strays.start_post_admission_maintenance()` before the
resume loop (v2 :175); `strays.reap_stray_keeper_channels().await` at the end (v2 :316);
`reconciliation.reconciled(now_ms)` where v2 calls onReconciled. WResume went BLOCKED before phase 2.
Lead was told (no objection received yet): if no reconcile pass is in the tree when wiring, add these at
the boot adoption step in boot_sequence.rs (after `adopt_survivors(..)`): `owners.heart.reconciliation.started()`
before adoption, `owners.heart.strays.start_post_admission_maintenance()` before adoption,
`owners.heart.strays.reap_stray_keeper_channels().await` after it, then
`owners.heart.reconciliation.reconciled(stack.clock.now_epoch_ms())`, and flag it in the report so the
reconcile owner moves them. Hazard to report: today `channel_history` is stubbed so adoption declines every
survivor; the sweep then reaps those survivors ~60 s after boot (v2 behaviour for untracked channels).

## Interfaces agreed with siblings
- WKUpdate: uses `SessionManager::begin_keeper_update_preparation()` (sync call, returns future) and
  `PreparationRollback::rollback()`, reads `keeper_update_prepared()`. Provides
  `crate::keeper_pool::KeeperPool::probe_runtime(self: &Arc<Self>) -> impl Future<Output = Result<KeeperRuntimeProbe, PoolError>> + Send + 'static`
  and `KeeperRuntimeProbe::observation(&self, reconciled_at_ms: i64) -> Option<KeeperRuntimeObservationV1>`
  (heartbeat_sources.rs calls exactly that; compile needs WKUpdate's keeper_pool/runtime_proof.rs; if absent,
  wait or ask WKUpdate). WKUpdate asserts the preparer-ordering half of v2 creation-gate tests 1–2 in
  tests/keeper_update_prepare.rs using `Harness::with_spawner` + a copy of ScriptedSpawner.
- W1Wire: `SessionManager::on_session_closed(Arc<dyn Fn(&SessionId)+Send+Sync>)` (session/closed_hooks.rs),
  fired after `sessions.forget`, no lock held; cwd events in session/cwd_events.rs (edit 7).
- WResume: see Reconcile wiring; also reads `manager.keeper_update_prepared()` for keeper-death /
  degraded suppression; owns emit_no_session burst (not W-HEART).

## Tests to run (after /tmp/wcheck.sh compile)
`/tmp/wcargo.sh test -p roost-worker --test heartbeat --test heartbeat_host_metrics --test channel_creation_gate --test session_git_ports --test stray_reap --test strays --test host_folder_facts --test host_listening_ports --test host_pr_status --test host_samples`
then `/tmp/wcargo.sh clippy -p roost-worker --all-targets -- -D warnings`. Likely compile nits to check:
`WorkersHeartbeatRequest` Debug (tests compare with `==` not assert_eq), `LogFields::set` value types,
`record.session_id()` returns `&SessionId`.

## Mutations planned (run each, watch the named test fail, revert, record file:line)
1. heartbeat.rs spawn loop: create `next` sleep AFTER `settled.send` or re-create inside select each loop → `never_overlaps_calls_and_schedules_the_next_attempt_from_settlement`.
2. heartbeat.rs `send`: delete the "reconciliation changed mid-probe" check → `drops_an_observation_whose_reconciliation_was_superseded_mid_beat`.
3. heartbeat.rs `attempt`: on sample error set host_metrics = None (drop last good) → `retains_the_last_good_metrics_when_collection_is_unknown`.
4. heartbeat.rs: `consecutive_misses` not reset on success → `counts_one_miss_per_settlement_resets_on_success_and_isolates_instances`.
5. heartbeat_metrics.rs: cache gate `<` → `<=` or cache ignored → `a_sample_is_reused_for_a_minute_and_then_replaced`; update `previous_net` on cached beats → `bandwidth_is_the_rate_between_the_last_two_real_samples`.
6. channel_creation_gate.rs `try_acquire`: ignore `preparations` → `preparation_drains_an_admitted_spawn_rejects_a_new_spawn_and_stays_closed_on_success`; `begin_preparation` skip `wait_for` → same test (preparation finished early); rollback without the `rolled_back` swap → `a_rollback_releases_only_its_own_preparation_once`; drop the `set_keeper_update_prepared(true)` → test 1's write-lane assert.
7. respawn.rs: remove the `_lease` line in `respawn_lost_child` → `a_failed_admitted_respawn_drains_before_preparation_and_rollback_reopens_admission`.
8. strays.rs: keep the strike on Reap → `a_reaped_channel_still_listed_starts_its_strikes_over`; `STRAY_STRIKES` compare `<=` → `a_just_spawned_channel_is_never_reaped_on_its_first_sighting`.
9. stray_reap.rs: tracked set built from nothing (every channel stray) → `a_stray_dies_after_two_sweeps_…`; `start_post_admission_maintenance` without the `is_some` guard → `the_periodic_sweep_starts_once_…`.
10. git_ports.rs `apply`: drop the `record.session_id() != session_id` guard → `a_reading_for_a_channel_now_held_by_another_session_is_dropped`; Branch compare against `record.git_branch` raw (Some(None) vs None) → `a_changed_branch_publishes_one_git_event…` (non-repo publishes).
11. samples.rs `sample_linux_net`: revert to `find_map(split_once(':')).filter(..)` → `every_interface_row_of_proc_net_dev_yields_counters`.
12. pr_status.rs rollup: use `conclusion` only (no `?? state`) → `an_entry_without_a_conclusion_is_judged_by_its_state`.

## Consumers of new values (for the commit body)
FolderReading/FolderFactsSink → session::git_ports::SessionFacts (production) ; SessionFolderHooks → git_ports (registers), respawn/resume/cwd_events (notify);
KeeperReconciliation → heartbeat `send` (reader), reconcile pass (writer); StraySweeper → reconcile pass + its own timer;
ChannelCreationGate/PreparationRollback → respawn.rs leases, WKUpdate preparer; `keeper_update_prepared()` → WKUpdate, WResume;
HeartbeatHandle → HeartOwners::shutdown; CHANNEL_CREATION_REFUSAL → admit_channel_creation refusal text;
tool_path::run_on_path/run_bounded → ports, pr_status, samples, samples_darwin, tailnet.

## Open questions / report items
- Task text said heartbeats carry git branch / ports / PR: v2 does not; they are session events (implemented). State this in the report.
- keeper_runtime in beats depends on WKUpdate's probe AND on reconciled() being called (reconcile owner).
- v2 register (install.ts) also sends the tailnet name; Rust register sends env only — parity gap outside this slice (host/tailnet.rs can serve it).
- Rust reconcile `sessions_list` sends no JWT (v2 does) — WResume/boot concern, noted only.
- README (lead-owned): list new modules; not-ported: host-sample-win32.ts (Windows paused).
