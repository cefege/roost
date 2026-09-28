worker: install the OMP/Pi agent integrations at boot (assets, proof, staged transaction)

Ports v2 apps/worker/src/agents/{install-integrations,integration-assets,
integration-assets.generated,integration-install-proof,
integration-install-transaction,standalone-integration}.ts. The OMP and Pi
plugins and report-transport.ts are embedded byte-for-byte from
crates/roost-worker/assets/, so they no longer depend on apps/ existing. They are
composed at runtime into standalone modules, and the result is byte-identical to
v2's composeStandaloneIntegration output: all three sha256 sums matched. The boot
installs them before the local door opens, as v2 main.ts:139-143 does. A refusal
is logged as a warning and never fails the boot.

Files: agents/integration_assets.rs (catalog + composition),
install_proof.rs (identities, ownership marker, NFC/case path key),
install_stage.rs (prepare/stage/cleanup, POSIX durable write),
install_mutation.rs (link-into-place, retire, reverse rollback),
install_transaction.rs (commit), install_integrations.rs (planning, report,
boot step).

Consumers: runtime/boot_sequence.rs calls install_agent_integrations_at_boot.
AgentIntegrationInstallReport.installed/.failed feed the boot info line, and
each refused target also gets its own warning. IntegrationTargetPlan.remove
feeds assert_integration_target_unchanged. PreparedDirectory.created feeds
cleanup_created_directories. Only tests set IntegrationInstallTestHooks.

Tests: 16 tests in agent_status_installer (10), agent_status_install_rollback
(3) and agent_status_integration_ownership (3). They cover every v2 case in
agent-status-installer.test.ts and agent-status-integration-ownership.test.ts,
plus these guards: env precedence, the spliced standalone module, and Linux
case-sensitivity. All of them install into a scratch HOME under temp_dir.

Mutations (each seen to fail, then reverted): see the WAgentsInstallP2 report.

Cross-owner/shared edits: workspace Cargo.toml adds
unicode-normalization = "0.1.25" (already in Cargo.lock).
crates/roost-worker/Cargo.toml adds the dependency, and Cargo.lock gains the
roost-worker -> unicode-normalization edge. agents/mod.rs gets six mod lines.
boot_sequence.rs gets one line.

Deviations: filesystem calls are synchronous std::fs run on spawn_blocking, and
the hooks are synchronous. Durability is POSIX-only: the staged file is created
with create_new and mode 0600, then fsynced along with the stage directory,
instead of v2's .tmp+rename inside the private stage. A refusal's reason string
drops v2's "Error: " prefix.

Replaced downstream arms: none.
