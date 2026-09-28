//! Ownership recognition for agent-integration assets already on disk, as v2
//! `apps/worker/tests/agents/agent-status-integration-ownership.test.ts` pins
//! it: an installed asset carries its marker below a spliced report-transport
//! preamble, so planning, adoption and the commit guard must all see a marker
//! ~100 lines into the file — and must still refuse unmarked files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod integration_install_support;

use std::fs;

use integration_install_support::{Scratch, installed_ids, omp_dir};
use roost_host::env::MapEnv;
use roost_platform::HostPlatform;
use roost_worker::agents::install_integrations::install_agent_integrations;
use roost_worker::agents::install_proof::{
    IntegrationTargetPlan, assert_integration_target_unchanged, has_integration_ownership,
    inspect_integration_target,
};
use roost_worker::agents::integration_assets::{
    AgentIntegrationAssetId, load_agent_integration_assets,
};

const OMP_MARKER: &str = "ROOST_INTEGRATION_ID=omp";
const OMP_MARKER_COMMENT: &str = "// ROOST_INTEGRATION_ID=omp ROOST_INTEGRATION_VERSION=2";

/// An installed asset in the shape Roost wrote it: the shared report transport
/// — a whole module, not a comment header — above the integration's own
/// header, putting the header line on file line 106.
fn transport_prefixed_asset(header_line: &str) -> String {
    let mut transport = vec![
        "// Shared delivery transport for the agent-status integrations (omp + pi).".to_owned(),
        "// Spliced in so the installed file resolves no imports of its own.".to_owned(),
        "import net from \"node:net\";".to_owned(),
    ];
    transport.extend((0..100).map(|idx| format!("const transportStep{idx} = {idx};")));
    format!(
        "{}\n\n// Roost-owned integration.\n{header_line}\nexport default function install(): void {{}}\n",
        transport.join("\n")
    )
}

#[test]
fn the_marker_is_recognized_in_any_comment_line_and_nowhere_else() {
    let installed = transport_prefixed_asset(OMP_MARKER_COMMENT);
    let marker_line = installed
        .split('\n')
        .position(|line| line.contains(OMP_MARKER))
        .unwrap()
        + 1;
    assert_eq!(marker_line, 106);
    assert!(has_integration_ownership(&installed, OMP_MARKER));

    let code_mention = transport_prefixed_asset(&format!("const claimed = \"{OMP_MARKER}\";"));
    assert!(!has_integration_ownership(&code_mention, OMP_MARKER));
    let near_miss = transport_prefixed_asset("// ROOST_INTEGRATION_ID=omp-reference");
    assert!(!has_integration_ownership(&near_miss, OMP_MARKER));
    let unmarked = transport_prefixed_asset("// hand-written extension");
    assert!(!has_integration_ownership(&unmarked, OMP_MARKER));
}

#[test]
fn adopts_and_overwrites_an_installed_asset_marked_below_the_file_head() {
    let home = Scratch::new("integration-ownership-adopt");
    let target = omp_dir(&home).join("roost-omp-agent-state.ts");
    fs::create_dir_all(omp_dir(&home)).unwrap();
    fs::write(&target, transport_prefixed_asset(OMP_MARKER_COMMENT)).unwrap();

    let report =
        install_agent_integrations(&MapEnv::new(), home.root(), HostPlatform::Linux).unwrap();

    let current = load_agent_integration_assets()
        .unwrap()
        .into_iter()
        .find(|asset| asset.spec.id == AgentIntegrationAssetId::OmpStatus)
        .unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), current.content);
    assert!(installed_ids(&report).contains(&"omp-status"));
    assert!(report.failed.is_empty());
}

#[test]
fn the_commit_guard_passes_a_target_marked_below_the_file_head() {
    let home = Scratch::new("integration-ownership-guard");
    let target = home.path("roost-omp-agent-state.ts");
    fs::write(&target, transport_prefixed_asset(OMP_MARKER_COMMENT)).unwrap();
    let existing = inspect_integration_target(&target, "agent integration target").unwrap();

    let guarded = assert_integration_target_unchanged(IntegrationTargetPlan {
        target: &target,
        ownership_marker: OMP_MARKER,
        existing: existing.as_ref(),
        remove: None,
    });

    assert!(guarded.is_ok(), "{guarded:?}");
}
