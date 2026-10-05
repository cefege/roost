//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "add_machine_enrollment.rs"]
mod add_machine_enrollment;
#[path = "agent_skill_document.rs"]
mod agent_skill_document;
#[path = "api_coordinator_calls.rs"]
mod api_coordinator_calls;
#[path = "api_verb_dispatch.rs"]
mod api_verb_dispatch;
#[path = "command_contract_coverage.rs"]
mod command_contract_coverage;
#[path = "command_tree_shape.rs"]
mod command_tree_shape;
#[path = "coord_inventory_query.rs"]
mod coord_inventory_query;
#[path = "daemon_boot_refusal.rs"]
mod daemon_boot_refusal;
#[path = "deploy_coordinator_release.rs"]
mod deploy_coordinator_release;
#[path = "deploy_installed_release.rs"]
mod deploy_installed_release;
#[path = "deploy_keeper_admission.rs"]
mod deploy_keeper_admission;
#[path = "deploy_keeper_classification.rs"]
mod deploy_keeper_classification;
#[path = "deploy_machine_transaction.rs"]
mod deploy_machine_transaction;
#[path = "deploy_release_build.rs"]
mod deploy_release_build;
#[path = "deploy_release_fetch.rs"]
mod deploy_release_fetch;
#[path = "deploy_release_path.rs"]
mod deploy_release_path;
#[path = "deploy_release_stage.rs"]
mod deploy_release_stage;
#[path = "deploy_remote_identity.rs"]
mod deploy_remote_identity;
#[path = "deploy_remote_web_bundle.rs"]
mod deploy_remote_web_bundle;
#[path = "dev_fan_out.rs"]
mod dev_fan_out;
#[path = "dev_stack_plan.rs"]
mod dev_stack_plan;
#[path = "doctor_digest_shape.rs"]
mod doctor_digest_shape;
#[path = "import_v2_copy.rs"]
mod import_v2_copy;
#[path = "import_v2_plan.rs"]
mod import_v2_plan;
#[path = "join_enrollment.rs"]
mod join_enrollment;
#[path = "join_script.rs"]
mod join_script;
#[path = "local_programs_keeper.rs"]
mod local_programs_keeper;
#[path = "push_fleet_plan.rs"]
mod push_fleet_plan;
#[path = "push_fleet_rollback.rs"]
mod push_fleet_rollback;
#[path = "push_keeper_admission.rs"]
mod push_keeper_admission;
#[path = "quickstart_dry_run.rs"]
mod quickstart_dry_run;
#[path = "quickstart_web_bundle.rs"]
mod quickstart_web_bundle;
#[path = "release_asset_names.rs"]
mod release_asset_names;
#[path = "self_link_repair.rs"]
mod self_link_repair;
#[path = "services_definition_text.rs"]
mod services_definition_text;
#[path = "services_deploy_first_install.rs"]
mod services_deploy_first_install;
#[path = "services_deploy_recovery.rs"]
mod services_deploy_recovery;
#[path = "services_deploy_rollback.rs"]
mod services_deploy_rollback;
#[path = "services_install_idempotence.rs"]
mod services_install_idempotence;
#[path = "services_linger.rs"]
mod services_linger;
#[path = "services_logrotate.rs"]
mod services_logrotate;
#[path = "services_web_bundle.rs"]
mod services_web_bundle;
#[path = "status_output_shape.rs"]
mod status_output_shape;
#[path = "update_keeper_gate.rs"]
mod update_keeper_gate;
#[path = "update_recovery.rs"]
mod update_recovery;
#[path = "update_release_decision.rs"]
mod update_release_decision;
#[path = "update_replace_bytes.rs"]
mod update_replace_bytes;
#[path = "update_self_replace.rs"]
mod update_self_replace;
#[path = "worker_boot_resolution.rs"]
mod worker_boot_resolution;
#[path = "worker_subcommand_boot.rs"]
mod worker_subcommand_boot;
