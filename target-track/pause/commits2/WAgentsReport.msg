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
