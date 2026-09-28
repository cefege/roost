worker: the agent report endpoint, protocol, server and pinned screen manifests

Ports v2 apps/worker/src/agents/{report-server,report-protocol,environment,
manifest-engine,manifests}.ts and the POSIX half of packages/host/src/local-endpoint.ts.
Integrations report over a capability-guarded UDS; the kernel peer PID plus a fresh
detector scan own identity; statuses reach the registry in the worker's own sequence
(max(last+1, now_ms*1000)); OMP references are acknowledged only after the durable
append. Every PTY carries the ROOST_AGENT_* overlay through the shell-spec resolver
(SessionEnvironmentOverlay); a refused endpoint refuses the spawn, as in v2.
The endpoint lives under the v3 worker data dir and its override is read from the
boot EnvSource (README "Deliberate deviations from v2": side-by-side safety with v2's
~/.roost/agent-report.sock). Manifests run with v2's JS regex classes.

Consumers: WorkerOwners::build starts AgentReportServer::start_for_worker over the
AgentStatusStack (detector, registry, reference gate) and the durable sink;
boot_sequence closes it (close_agent_report) before the door, as v2 main.ts:348-354.
AgentReportEnvironment is built once in session_stack::build (new last parameter
&AgentReportSite from WorkerBoot::agent_report) and shared with the detector.
AGENT_REPORT_MAX_LINE_BYTES moved from agents/mod.rs to report_protocol.rs.

Mutations (each seen to fail, then reverted):
- manifest_regex.rs ('w', false) -> raw \w: word_digit_and_boundary_classes_are_ascii_as_in_javascript
- manifest_engine.rs WholeRecentWithoutCurrentPromptMarker -> always content: codex_idle_at_its_composer_ignores_an_approval_echoed_earlier_in_the_transcript
- environment.rs from_env filter-empty before SOCKET_PATH fallback: the_override_is_read_from_the_boot_environment_with_v2_precedence
- shell_spec_resolver.rs overlay Err -> unwrap_or_default: an_overlay_refusal_refuses_the_launch_contract
- report_connection.rs peer pid ignored (0): rejects_a_different_peer_process_reporting_for_the_live_agent
Planned but not completed (budget): capability-check drop, request-count move,
seq ordering, non-omp reference check, manifest priority <=, pinned() order swap,
capability re-mint per call.

Throwaway proofs (run once, deleted): zod first-issue parity on 45 oracle lines
(all match); the real v2 TS report transport against the Rust server (blocked +
"waiting on approval" published; reference acknowledged; one AgentReference event).

Replaced arms: none (a local socket, not a downstream kind).
Cross-owner edits: none. Shared edits: agents/mod.rs, host/mod.rs, session/spawn.rs,
runtime/{boot,session_stack,boot_sequence,owners}.rs, tests/{boot_adoption_gate,
terminal_input_e2e,shell_spec_resolution}.rs.

Mutation batch at resume (WorkerLead; mutbatch.py: co-applied mutants never share a file
or a test binary; KILLED = the named guard failed under the mutant; every file restored):
- M1 roost-worker agents/report_connection.rs: `.verify_capability(session_id, request.capability())` -> `.verify_capability(session_id, request.capability()) && false` => FAILS refuses_a_capability_minted_for_another_session
- M8 roost-worker agents/manifest_engine.rs: `incumbent.priority < rule.priority` -> `incumbent.priority > rule.priority` => FAILS honors_blocker_priority_visible_idle_and_skip_state_screens
- M11 roost-worker agents/environment.rs: `&[SESSION_CAPABILITY_CONTEXT, session_id.as_bytes()],` -> `&[SESSION_CAPABILITY_CONTEXT, session_id.as_bytes(), format!("{:?}"...` => FAILS a_session_capability_survives_a_worker_restart_and_is_per_session
- M2 roost-worker agents/report_connection.rs: `&& let Screened::Admit(request) = self.screen(line) { self.admissio...` -> `&& let Screened::Admit(_request) = self.screen(line) { } return Som...` => FAILS a_second_request_line_is_refused_while_the_first_still_lands
- M9 roost-worker agents/manifests.rs: `compile(BuiltinAgentId::Codex)?, compile(BuiltinAgentId::Gemini)?,` -> `compile(BuiltinAgentId::Gemini)?, compile(BuiltinAgentId::Codex)?,` => FAILS every_agent_is_evaluated_against_its_own_manifest
- M3 roost-worker agents/report_admission.rs: `let seq = last_seq.saturating_add(1).max(wall_clock_micros());` -> `let seq = *last_seq;` => FAILS accepts_state_with_fresh_worker_derived_identity_and_ordering
- M4 roost-worker agents/report_admission.rs: `if identity.agent_id != BuiltinAgentId::Omp {` -> `if identity.agent_id == BuiltinAgentId::Codex {` => FAILS durably_acknowledges_omp_set_replace_and_clear_after_fresh_pid_proof

Series: one of the wave 2+3 per-slice commits (ProtoDoor..WAgentsPrompt). A file shared by
several slices lands whole in the FIRST slice commit that lists it, so intermediate commits
may not build; the track gate (tests x2, clippy, lint, fmt) ran on the series tip.
