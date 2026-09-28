# WAgentsReport — phase-2 hand-off (self-sufficient)

Slice: W-AGENTS **report path** of Stage 2W (plan: docs/v3-handoff/roost-v3-finish-and-cutover-plan.md "### Stage 2W").
Ports v2 `apps/worker/src/agents/{report-server,report-protocol,environment,manifest-engine,manifests}.ts`
(+ the POSIX half of `packages/host/src/local-endpoint.ts`, which nothing in Rust had).
NOT this slice (agreed with WAgentsInstall): `report-transport.ts` (embedded verbatim as
`crates/roost-worker/assets/report-transport.ts` and `REPORT_TRANSPORT_SOURCE` in WAgentsInstall's
`agents/integration_assets.rs`) and `standalone-integration.ts` (compose fn, same file). v2 test
`agent-reference-report-transport.test.ts` belongs to WAgentsInstall too. Do NOT create
`report_transport.rs` / `standalone_integration.rs`.

Status at hand-off: phase 1 done. Nothing compiled with cargo. Checked: `rustc` compiles the
const-table pattern used by manifests.rs (nested `&[...]` of const-fn calls promotes to 'static);
regex-syntax 0.8 source allows `(?-u:\b)` in UTF-8 mode and `\uXXXX` escapes. All `src/` drafts are
rustfmt'd (rustfmt --edition 2024, run on the draft files only; the manifest tables carry
`#[rustfmt::skip]`, precedent `crates/roost-coord/src/rpc/method_route_rows.rs`). The three test
files that `#[path]`-include `credential_support/scratch.rs` could not be rustfmt'd in the draft
tree: run `~/.cargo/bin/rustfmt --edition 2024 --check <file>` on them after moving (never cargo fmt).

## 1. Files (draft → repo, same relative path under crates/roost-worker/)

| repo path | lines | ports | purpose / production reader |
|---|---|---|---|
| src/host/local_endpoint.rs | 235 | packages/host/src/local-endpoint.ts (POSIX) | `LocalEndpoint{address,capability,capability_path}` (Debug redacts capability), `resolve_local_endpoint(name,data_dir)` (mkdir 0700, capability file create_new 0600 from /dev/urandom, EEXIST → read winner), `prepare/secure/cleanup_local_endpoint`, `verify_local_endpoint_capability` (sha256 both + constant-time), consts `LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES`(64Ki) `_TIMEOUT`(2s) `LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS`(16) `LOCAL_ENDPOINT_KIND_UDS`. Readers: agents::environment, agents::report_server/report_connection. (WAttach/WDoor may reuse `verify_local_endpoint_capability` — v2 attachment-grants.ts / local-terminal-grants.ts import it.) |
| src/agents/environment.rs | 238 | environment.ts | `AgentReportSite{data_dir, configured}` + `from_env(env,&dyn EnvSource, data_dir)` (ENDPOINT wins over SOCKET_PATH even when empty; empty = no override); `AgentReportEnvironment` (owner; no statics): `resolve(&site)` (keeps Err, logs), `for_endpoint(LocalEndpoint)` (no I/O, for tests), `endpoint()`, `session_overlay(&str)->Result<Vec<(String,String)>,String>` (ROOST_AGENT_ENDPOINT, _KIND=uds, _CAPABILITY, ROOST_SESSION_ID, ROOST_AGENT_SOCKET_PATH), `verify_capability(session,received)`, `release_agent_status_capabilities(&SessionId)->usize`; per-session capability = hex(HMAC-SHA256(key=endpoint capability text, "roost-agent-report-session\0"+session)) cached per session; hand-rolled RFC 2104 HMAC over sha2 (no hmac dep; RFC 4231 vectors unit-tested). impl `crate::session::spawn::SessionEnvironmentOverlay`. Readers: shell-spec resolver (overlay), report_connection (verify), WAgentsDetect detector close_session (release). |
| src/agents/report_protocol.rs | 255 | report-protocol.ts | `AGENT_REPORT_MAX_LINE_BYTES` (MOVED here from agents/mod.rs), `REPORTED_STATE_UNKNOWN_REASON`, `AgentStatusReportRequest`, `AgentReferenceReportRequest` (reference completed to `AgentConversationReferenceV1{schema_version 1, agent_id "omp"}` + `.check()`), `AgentIntegrationRequest{Report,Reference}`, `RequestRefusal{InvalidJson, InvalidRequest{detail}}`, `parse_agent_integration_request(&str)`. Emulates zod 3 first-issue wording/order (discriminator → version → capability → params(shape order, then unknown keys) → root unknown keys; message max counted in UTF-16 units). Reader: report_connection. |
| src/agents/report_admission.rs | 183 | report-server.ts admission half | pub traits `ReportingAgentLookup` (fresh identity for attested pid) and `IntegrationReportSink` (+ forwarding impls for WAgentsDetect's `AgentScreenDetector` / `AgentStatusRegistry`); `pub(super) ReportAdmission` with `admit_report` (tokio Mutex<u64> serializes like v2 admissionTail; seq = max(last+1, now_ms*1000); > 2^53-1 → fault → internal_error; registry false → "stale_report") and `admit_reference` (inside `AgentReferenceAdmissionGate::run_exclusive`; non-omp → "unsupported_agent"; `emit_durable_agent_reference` then info log). |
| src/agents/report_connection.rs | 328 | report-server.ts connection half | per connection: peer pid via `LocalPeerProcessIdReader::read(&UnixStream)` (None → warn, drop), unauthenticated slot (16, RAII), 2 s auth deadline, 64 KiB unauth-byte cap, 2×32 KiB buffer cap, ONE request per connection (a second line → too_many_requests, but the first line is still screened+admitted silently — v2 ordering), size/JSON/schema/capability screening sync, admission async; peer EOF → admission completes unanswered; reference fault → silence (ambiguous), report fault → internal_error. Response JSON field order ok,error,detail via serde struct. |
| src/agents/report_server.rs | 215 | report-server.ts lifecycle | `AgentReportServerOptions{environment, detector: Arc<dyn ReportingAgentLookup>, registry: Arc<dyn IntegrationReportSink>, event_sink: Arc<dyn SessionEventSink>, reference_admission: AgentReferenceAdmissionGate, peer_process_id_reader: Option<LocalPeerProcessIdReader>, socket_path: Option<PathBuf>}`; `AgentReportServer::start(opts) -> Result<Self, AgentReportServerError>` (SYNC, must be inside the tokio runtime; prepare → bind → chmod 0600; cleanup on failure), `path()`, `async close(self)` (stop accept loop, await open connections, close owned reader, rm socket), Drop aborts the accept task. Accept errors pause 50 ms (no hot loop). Reader: runtime/owners.rs. |
| src/agents/manifest_engine.rs | 306 | manifest-engine.ts | `ManifestGate` (contains/regex/line_regex/all/any/not), `ManifestRegion` enum (only the regions the pinned tables use: WholeRecent, OscTitle, OscProgress, AfterLastPromptMarker, WholeRecentWithoutCurrentPromptMarker, BottomNonEmptyLines(n), TopNonEmptyLines(n)), `ManifestRule{id,state:Option<AgentRuntimeState>(None=unknown),priority,region,visible,skip_state_update,gate}`, `AgentManifest{id,rules}`, `DetectionInput<'a>{screen,osc_title,osc_progress: &str}`, `ManifestDetection{state,visible_idle,visible_blocker,visible_working,skip_state_update,matched_rule_id:Option<&'static str>}`, `CompiledManifest::compile/id`, `evaluate_manifest(&CompiledManifest,&DetectionInput)` (infallible; highest priority wins, earlier wins ties, no match = idle). Reader: WAgentsDetect detector. |
| src/agents/manifest_regex.rs | 122 | compileHerdrRegex | `compile_herdr_regex` rewrites to JS `u`-flag semantics: `\w \W \d \D \s \S` → ASCII/JS sets, `\b` → `(?-u:\b)`, `.` → `[^\n\r\x{2028}\x{2029}]`, `[ & ~` escaped inside classes. Unit tests inside. |
| src/agents/manifest_syntax.rs | 98 | manifests.ts literal shape | `pub(super) const fn` constructors: manifest, blocked/working/idle/unknown, `.at(region) .visible() .skipping_state_update()`, contains/regex/line_regex/any/all. Reader: manifests.rs only. |
| src/agents/manifests.rs | 320 | manifests.ts | the ten pinned tables (`#[rustfmt::skip]`), `agent_manifest(BuiltinAgentId) -> &'static AgentManifest`, `ManifestCompileError{agent,source}`, `AgentManifests::pinned() -> Result<Self,_>` (array in declaration order) and `get(agent) -> &CompiledManifest` (indexes `agent as usize`). Reader: WAgentsDetect (owners.rs builds `AgentManifests::pinned()?` once). Aliases/version of v2 tables dropped (no reader; version kept as a doc comment per table). |
| tests/agent_report_support/mod.rs | 243 | test fakes | PeerPid (NativePeerProcessIdQuery), Detector (ReportingAgentLookup; identity only for SESSION + matching pid; records attested pids), Reports (IntegrationReportSink; records, optional forward), Ledger (SessionEventSink over event_store::Store, records events, appends as DurableEventKind::AgentReference), line builders, `request()`, `start()` (sync). Uses `report.clone()` → needs `IntegrationStatusReport: Clone` (else record fields). |
| tests/agent_report_server.rs | 215 | agent-status-report-server.test.ts | 6 tests (below) |
| tests/agent_report_peer.rs | 74 | same file, "rejects a different peer process" | re-execs the test binary (`--exact reports_from_a_child_process`, env ROOST_V3_TEST_REPORT_CHILD_ENDPOINT/BODY) with the NATIVE reader; pattern = tests/retire_support/child.rs |
| tests/agent_report_environment.rs | 99 | "agent report environment" + new edges | 4 tests |
| tests/agent_manifest_rules.rs | 302 | agent-status-manifest-rules.test.ts + agent-status.test.ts "pinned manifest engine" | 14 tests |
| throwaway/ (NOT for the repo) | | | oracle.ts + oracle.json (zod 3.25.76 first-issue messages for 50 inputs; zod is in ~/.bun/install/cache, run with /tmp/wagents-oracle/node_modules/zod symlink), transport_smoke.ts (drives the REAL v2 TS transport against the Rust server) |

## 2. Interfaces agreed with siblings (use these exact names; confirm against their landed code)

WAgentsDetect (`crate::agents::*`, drafts at target-track/drafts/WAgentsDetect if present):
- `registry::IntegrationStatusReport { session_id: SessionId, agent_id: BuiltinAgentId, process_id: u32, state: AgentRuntimeState, message: Option<String>, seq: u64, active: bool }`; `AgentStatusRegistry::report_integration(&self, IntegrationStatusReport) -> bool`; `AgentStatusRegistry::new(AgentStatusRegistryOptions { publish: Arc<dyn AgentStatusPublisher>, clock: Arc<dyn EventClock>, lease_ms: i64 }) -> Result<Arc<AgentStatusRegistry>, MintError>`; `trait AgentStatusPublisher { fn publish(&self, AgentStatusUpdate) }`. Their tests/agent_status_support/mod.rs has `Published` + `registry_harness`/`default_registry()` (may replace my local `Published`).
- `detector::AgentScreenDetector::reporting_agent_for_session(&self, &SessionId, reporter_pid: u32, abort: Option<process_scan::ScanAbort>) -> crate::uplink::OwnerFuture<Option<process_scan::AgentProcessIdentity>>` (I pass None); `process_scan::AgentProcessIdentity { agent_id: BuiltinAgentId, pid: u32, foreground: Option<process_tree::AgentForegroundJob> }`.
- `peer_process_id::LocalPeerProcessIdReader::{native() -> Self, with_query(Arc<dyn NativePeerProcessIdQuery>) -> Self, read(&self, &tokio::net::UnixStream) -> Option<u32>, close(&self)}`; `trait NativePeerProcessIdQuery: Send+Sync { fn read(&self, socket: &tokio::net::UnixStream) -> Result<Option<i64>, String>; fn close(&self) {} }`. NO `available()` (v2 `peer_process_attestation_unavailable` warning has no Rust trigger — N/A).
- They consume from me: `manifest_engine::{DetectionInput, ManifestDetection, CompiledManifest, evaluate_manifest}`, `manifests::{AgentManifests, agent_manifest}`, `environment::AgentReportEnvironment::{for_endpoint, session_overlay, release_agent_status_capabilities}`. Their detector takes `manifests: Arc<AgentManifests>` and `environment: Arc<AgentReportEnvironment>`; owners.rs holds `agents: AgentStatusStack { registry: Arc<AgentStatusRegistry>, detector: Arc<AgentScreenDetector> }` built by them.
WAgentsPrompt (`crate::agents::reference_admission`):
- `#[derive(Clone, Debug, Default)] AgentReferenceAdmissionGate::new()`, `async fn run_exclusive<F, Fut, T>(&self, F) -> T where F: FnOnce() -> Fut, Fut: Future<Output=T>`.
- `async fn emit_durable_agent_reference(sink: &dyn SessionEventSink, session_id: &SessionId, reference: Option<&AgentConversationReferenceV1>) -> Result<(), SessionEventError>` (adds `DurableEventKind::AgentReference`).
- Production handles: gate built ONCE in runtime/owners.rs (clones to me, WAgentsDetect, WResume); sink = `stack.manager.durable_event_sink() -> Arc<dyn SessionEventSink>` (they add it in session/lifecycle.rs).
WAgentsInstall: owns report-transport asset + compose (see top).

## 3. Lead-approved decision (record in README)
Endpoint lives under the v3 WORKER DATA DIR (`boot.data_dir`, e.g. ~/.local/share/RoostWorkerV3/agent-report.{cap,sock}), the
ROOST_AGENT_ENDPOINT / ROOST_AGENT_SOCKET_PATH override is read from the BOOT EnvSource (WorkerBoot::resolve), never
ProcessEnv; PTY env names unchanged. Reason: v2 POSIX uses ~/.roost/agent-report.{cap,sock}; its start rm+binds and its
shutdown rm's the socket, so v3 on that path would steal v2's live socket in the Stage-4 side-by-side run and v2's
disable (4.8) would delete v3's; in-process Rust boot tests would clobber the live v2 socket (this host's shells carry
ROOST_AGENT_ENDPOINT=/home/almalinux/.roost/agent-report.sock from the v2 PTY). v2 itself used workerDataDir on win32.

## 4. Edits to existing files (re-read each right before editing; siblings edit concurrently)
1. `src/agents/mod.rs`: add after the header/`BuiltinAgentId` block (keep siblings' lines): `pub mod environment; pub mod manifest_engine; mod manifest_regex; mod manifest_syntax; pub mod manifests; pub mod report_admission; mod report_connection; pub mod report_protocol; pub mod report_server;`. DELETE the `AGENT_REPORT_MAX_LINE_BYTES` doc+const at the file end (moved to report_protocol.rs; only user). Update docs/v3-handoff/worker-v2-map.md only if the lead asks (lines 260/360 cite the old place).
2. `src/host/mod.rs`: add `pub mod local_endpoint;` between `pub mod jwt;` and `pub mod openssh_key;`.
3. `src/session/spawn.rs`: right after `pub trait ShellSpecResolver { … }` (its doc already says "must apply the agent-status environment overlay") add:
   ```rust
   /// The per-session variables every launch contract carries — the agent
   /// report endpoint and its capability, derived from the session id. An `Err`
   /// refuses the spawn: a PTY must not carry an endpoint nobody serves.
   pub trait SessionEnvironmentOverlay: Send + Sync {
       fn session_overlay(&self, session_id: &str) -> Result<Vec<(String, String)>, String>;
   }
   ```
4. `src/host/shell_spec_resolver.rs` (390 lines; keep ≤400): import `std::sync::Arc` and `crate::session::spawn::SessionEnvironmentOverlay`; field `overlay: BTreeMap<String, String>` → `overlay: Option<Arc<dyn SessionEnvironmentOverlay>>` (keep its doc); `new()` → `overlay: None`; `with_overlay(mut self, overlay: Arc<dyn SessionEnvironmentOverlay>) -> Self { self.overlay = Some(overlay); self }` (keep doc); in `resolve()` replace `for (key, value) in &self.overlay {` with
   ```rust
   let overlay = match &self.overlay {
       Some(overlay) => overlay.session_overlay(session_id)?,
       None => Vec::new(),
   };
   for (key, value) in overlay {
   ```
   and inside the loop `env.insert(key, value);` (owned now); the keeper-key strip/warn stays unchanged.
5. `tests/shell_spec_resolution.rs` `a_keeper_control_key_is_stripped_from_an_overlay_too` (:326-359): replace the array argument with a test-local `struct FixedOverlay(Vec<(String,String)>); impl SessionEnvironmentOverlay for FixedOverlay { fn session_overlay(&self, _: &str) -> Result<Vec<(String,String)>,String> { Ok(self.0.clone()) } }` passed as `Arc::new(FixedOverlay(vec![...same two pairs...]))`; assertions unchanged. ADD `an_overlay_refusal_refuses_the_launch_contract`: overlay returning `Err("ROOST_AGENT_ENDPOINT must be an absolute UDS path".into())` → `resolver.resolve(folder, "s")` is `Err` with that text.
6. `src/runtime/boot.rs`: WorkerBoot gets (after `force_live_retire`)
   ```rust
   /// Where the agent report endpoint lives and any override of its address,
   /// read from the boot environment so a test boot never reaches the
   /// operator's own endpoint.
   pub agent_report: crate::agents::environment::AgentReportSite,
   ```
   and in `resolve()` the literal gets `agent_report: AgentReportSite::from_env(env, support.clone()),` (put it BEFORE `data_dir: support,` or clone). Also any test helper building WorkerBoot by literal (none found: tests use `WorkerBoot::resolve`).
7. `src/runtime/session_stack.rs`: `build(..)` gains a last parameter `agent_report: &crate::agents::environment::AgentReportSite`; before the resolver: `let agent_environment = Arc::new(AgentReportEnvironment::resolve(agent_report));`; resolver becomes `HostShellSpecResolver::for_this_host(..).map_err(..)?.with_overlay(Arc::clone(&agent_environment) as Arc<dyn SessionEnvironmentOverlay>)` (keep the existing map_err closure); `SessionStack` gains `/// The agent report endpoint every PTY is pointed at (v2 environment.ts); the report server and the detector share it. pub agent_environment: Arc<AgentReportEnvironment>,` and the returned literal sets it.
8. `src/runtime/boot_sequence.rs`: the `session_stack::build(` call gets one more argument `&boot.agent_report,`. Nothing else here (file must stay ≤400).
9. `src/runtime/owners.rs` (W1Wire's `WorkerOwners`; WAgentsDetect/WAgentsPrompt add their handles in the same `build`): after the agent status stack and the reference gate exist (v2 main.ts:251-261 order: after service health, before reconcile), add field `agent_report: Option<AgentReportServer>` and
   ```rust
   // v2 main.ts:251-261: a report server that cannot start is logged, not fatal.
   let agent_report = match AgentReportServer::start(AgentReportServerOptions {
       environment: Arc::clone(&stack.agent_environment),
       detector: Arc::clone(&agents.detector) as Arc<dyn ReportingAgentLookup>,
       registry: Arc::clone(&agents.registry) as Arc<dyn IntegrationReportSink>,
       event_sink: stack.manager.durable_event_sink(),
       reference_admission: reference_gate.clone(),
       peer_process_id_reader: None,
       socket_path: None,
   }) {
       Ok(server) => Some(server),
       Err(error) => {
           tracing::warn!(%error, "the agent report server could not start; integrations cannot report");
           None
       }
   };
   ```
   Shutdown (v2 main.ts:348-354 closes it on SIGTERM): `AgentReportServer::close` is async. Either make `WorkerOwners::shutdown` `async` and add `if let Some(server) = self.agent_report { server.close().await; }` (caller in boot_sequence `.await`s it), or add `pub async fn close_agent_report(&mut self)` called after `link.run` returns. Pick whichever the landed owners.rs/boot_sequence shape allows with the fewest boot_sequence lines. Dropping without close only aborts the accept loop (socket file stays, like a v2 crash).
10. `crates/roost-worker/README.md` (does not exist at hand-off; WAttach may create it — re-read): section `## Deliberate deviations from v2` with a bullet for §3 (endpoint dir + boot-env override + reasons).
No Cargo.toml/Cargo.lock/cross-crate edits (regex, sha2, serde, serde_json, tokio net/process/time, thiserror, futures-util all present). No link_drain arm is replaced by this slice (the report path is a local socket, not a downstream kind).

## 5. Tests (run: `/tmp/wcargo.sh test -p roost-worker --test <name>` and `--lib agents::` / `--lib host::local_endpoint`)
- tests/agent_report_server.rs ← v2 agent-status-report-server.test.ts: `accepts_state_with_fresh_worker_derived_identity_and_ordering` (REAL registry: acceptance "a report line over the transport reaches the registry"), `rejects_unavailable_identity_caller_selected_identity_and_malformed_state`, `caps_oversized_local_input`, `durably_acknowledges_omp_set_replace_and_clear_after_fresh_pid_proof`, plus new edges `refuses_a_capability_minted_for_another_session`, `a_second_request_line_is_refused_while_the_first_still_lands`.
- tests/agent_report_peer.rs ← "rejects a different peer process reporting for the live agent" (+ helper test `reports_from_a_child_process`, no-op unless its env is set).
- tests/agent_report_environment.rs ← "exports the report endpoint under the documented POSIX socket name" + `a_session_capability_survives_a_worker_restart_and_is_per_session`, `an_endpoint_override_must_be_an_absolute_socket_path`, `the_override_is_read_from_the_boot_environment_with_v2_precedence`.
- tests/agent_manifest_rules.rs ← all 10 SCREEN_CASES of agent-status-manifest-rules.test.ts + agent-status.test.ts "pinned manifest engine" (3) + `every_agent_is_evaluated_against_its_own_manifest`.
- unit: manifest_regex.rs (4 JS-dialect tests), environment.rs (HMAC RFC 4231 vectors), local_endpoint.rs (verify, endpoint name shape).
- tests/shell_spec_resolution.rs edits (§4.5).
- Not mine: agent-status-screen-gate "closeSession evicts the session's cached report capability" (WAgentsDetect ports it using `AgentReportEnvironment::for_endpoint`), agent-reference-report-transport + integration ownership/integrations tests (WAgentsInstall), stable-transitions (WAgentsDetect).
Throwaway proofs (run once, do not commit, delete after):
- zod parity: a temporary test that for every entry of throwaway/oracle.json asserts `parse_agent_integration_request(line)` gives Ok when `ok`, else `InvalidRequest{detail}` == oracle detail. Known expected diffs: none in the table (the oracle's reference schema is a simplification; the unknown-key LIST order differs only with 2+ unknown keys — serde_json Map is sorted, JS is insertion order).
- real TS transport: temporary test starting the server with the real registry, a Detector that accepts any attested pid as Omp, native reader; spawn `~/.bun/bin/bun throwaway/transport_smoke.ts` with SMOKE_ENDPOINT=server.path(), SMOKE_CAPABILITY=capability(env, SESSION), SMOKE_SESSION=SESSION; expect a registry publish with state blocked + message "waiting on approval", and stdout `SMOKE_REFERENCE {"status":"acknowledged"}` with one AgentReference event in the Ledger.

## 6. Mutations to perform (mutate → run → see it fail → revert; report each)
1. report_connection.rs `screen()`: drop the `verify_capability` check (always authenticate) → `refuses_a_capability_minted_for_another_session` fails.
2. report_connection.rs `receive()`: move `request_count += 1` below the blank-line check / or screen the first line before counting → `a_second_request_line_is_refused_while_the_first_still_lands` fails (answer differs or registry count 0).
3. report_admission.rs `admit_report`: `let seq = last_seq.saturating_add(1)` → `wall_clock_micros()` only → ordering assert in `accepts_state…` can fail (two reports in one ms) — if flaky, instead mutate `*last_seq = seq` away and assert seq strictly increases.
4. report_admission.rs `admit_reference`: remove the `!= BuiltinAgentId::Omp` check → durable test fails (4 events / ok instead of unsupported_agent).
5. report_connection.rs: `peer_reader.read` result ignored (use 0) → agent_report_peer test fails (attested pid ≠ child pid).
6. manifest_regex.rs: `('w', false)` arm → pass `\w` through → `word_digit_and_boundary_classes_are_ascii_as_in_javascript` fails.
7. manifest_engine.rs `select_region` `WholeRecentWithoutCurrentPromptMarker` → always `content` → `codex_idle_at_its_composer…` fails (weak_blocker).
8. manifest_engine.rs `evaluate_manifest`: `incumbent.priority < rule.priority` → `<=` → `copilot_background_agent_wait_outranks…`/priority tests fail (tie/priority precedence).
9. manifests.rs `pinned()`: swap two array entries → `every_agent_is_evaluated_against_its_own_manifest` fails.
10. environment.rs `AgentReportSite::from_env`: `.or_else(..)` then filter order swapped (filter empty before fallback) → `the_override_is_read_from_the_boot_environment_with_v2_precedence` fails.
11. local_endpoint.rs `load_or_create_capability`: mint a fresh capability every call → `a_session_capability_survives_a_worker_restart…` fails.
12. shell_spec_resolver.rs: ignore overlay Err (`unwrap_or_default`) → `an_overlay_refusal_refuses_the_launch_contract` fails.

## 7. Consumers of new values (for the report)
AGENT_REPORT_MAX_LINE_BYTES → report_connection; REPORTED_STATE_UNKNOWN_REASON → report_protocol detail; RequestRefusal variants → report_connection answers; admission refusals ("reporter_identity_mismatch","stale_report","unsupported_agent") → reply `error`; AdmissionFault → report internal_error / reference silence; ManifestDetection fields → WAgentsDetect stable detection (state/visible_*/skip_state_update) and tests (matched_rule_id); ManifestRegion variants → select_region; AgentReportSite → session_stack/boot; AgentReportEnvironment → resolver overlay, report server, detector; SessionEnvironmentOverlay → HostShellSpecResolver::resolve; LOCAL_ENDPOINT_* → report_connection; AgentReportServerError → owners.rs warn.

## 8. Parity gaps / notes for the final report
- Endpoint path deviation (§3, lead-approved).
- zod detail: unrecognized-key list order sorted (serde_json) vs JS insertion order; JSON with lone surrogates is invalid_json here (serde_json) but accepted by JSON.parse.
- v2 per-connection re-authentication branch (report-server.ts:221-228) is unreachable with MAX_REQUESTS_PER_CONNECTION=1 and is not ported.
- v2 `peer_process_attestation_unavailable` has no Rust trigger (tokio peer_cred has nothing to load).
- Unused manifest regions (before_current_prompt_marker, current_prompt_block_marker, after_current_prompt_block_marker, prompt_box_body, above_prompt_box, last_non_empty_above_prompt_box, after_last_horizontal_rule, bottom_lines) not ported: no pinned rule reads them; the enum makes a future one a compile error instead of v2's silent "".
- v2 re-resolved the endpoint on every spawn; v3 resolves once at boot (a cap file rewritten at runtime is not picked up — v2's server would have rejected those spawns anyway).
- Windows named-pipe half of local-endpoint.ts and win32 env case-folding branch not ported (Windows paused).

## 9. Commit message draft
`worker: the agent report endpoint, protocol, server and pinned screen manifests`
Body: ports v2 agents/{report-server,report-protocol,environment,manifest-engine,manifests}.ts and the POSIX half of
host/local-endpoint.ts. Integrations report over a capability-guarded UDS; kernel peer PID + a fresh detector scan own
identity; statuses reach the registry in the worker's own order; OMP references are acknowledged only after the durable
append. Every PTY carries the ROOST_AGENT_* overlay via the shell-spec resolver (a refused endpoint refuses the spawn, as
v2). Endpoint moved under the v3 worker data dir (side-by-side safety; README). Manifests run with v2's JS regex classes.
Mutations: <fill from §6 results>. Consumers: §7.
Paths: crates/roost-worker/src/host/{local_endpoint.rs,mod.rs,shell_spec_resolver.rs}, src/agents/{mod.rs,environment.rs,report_protocol.rs,report_admission.rs,report_connection.rs,report_server.rs,manifest_engine.rs,manifest_regex.rs,manifest_syntax.rs,manifests.rs}, src/session/spawn.rs, src/runtime/{boot.rs,session_stack.rs,boot_sequence.rs,owners.rs}, tests/{agent_report_server.rs,agent_report_peer.rs,agent_report_environment.rs,agent_manifest_rules.rs,agent_report_support/mod.rs,shell_spec_resolution.rs}, crates/roost-worker/README.md.

## 10. Open items for the phase-2 agent
- Confirm sibling signatures against landed code (§2); adapt imports (`crate::agents::detector`, `process_scan`, `peer_process_id`, `registry`, `reference_admission`) if module names differ.
- `IntegrationStatusReport` must be `Clone` for tests/agent_report_support (or record fields instead).
- The admission async block is annotated `Ok::<_, AdmissionFault>(None)`; keep if inference needs it.
- clippy: workspace lints warn `missing_debug_implementations` on exported types — all pub structs here have Debug (manual ones redact capabilities).
