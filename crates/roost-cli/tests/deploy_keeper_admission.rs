//! Whether a deploy may stage a release onto a target, and what it refuses when
//! the answer is not proven. These are the cases that cost PTYs.
//!
//! The pair that matters most is the first two: a coordinator row that stopped
//! refreshing, and a target that cannot be asked anything. Both describe machines
//! this command exists to repair, and both used to be a gate that refused exactly
//! them — so each case here runs the coordinator's own claim through the
//! target's own evidence and asserts the decision, not the claim.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_cli::deploy::admission::{
    AdmissionOutcome, keeper_admission_staging, worker_matches_target,
};
use roost_cli::deploy::keeper_step::decide;
use roost_cli::deploy::target_evidence::{
    TargetEvidence, installed_service_verdict, parse_target_evidence,
    target_worker_evidence_command,
};
use roost_cli::status::report::WorkerStatus;
use roost_host::HostPlatform;
use roost_protocol::keeper_update::{
    INCOMPATIBLE_WITH_LIVE_SESSIONS, KEEPER_EMPTY_BINDING_DIGEST, KEEPER_RESTART_REQUIRED,
    KeeperContractV1, KeeperRuntimeObservationV1, UNPROVEN, WORKER_ONLY_SAFE,
    classify_keeper_update,
};
use serde_json::json;

const NOW: i64 = 1_781_900_000_000;
const RUNNING_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TARGET_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// The one digest a provably empty keeper reports, taken from the constant that
/// owns it rather than restated: a literal here would be a second answer to "what
/// does an empty keeper hash to", and a replace-empty admission proved against
/// the wrong one would refuse every healthy machine.
const EMPTY_DIGEST: &str = KEEPER_EMPTY_BINDING_DIGEST;

fn keeper(channel_count: u32, digest: &str) -> KeeperRuntimeObservationV1 {
    KeeperRuntimeObservationV1::parse(&json!({
        "schema_version": 1,
        "running_contract": {
            "protocol_version": 3,
            "supported_features": [],
            "required_features": [],
            "implementation_digest": RUNNING_DIGEST,
            "platform": "linux",
            "arch": "x86_64",
            "build_sha": "b1d1836a"
        },
        "keeper_pid": 4242,
        "keeper_epoch": "6f1c2d3e-4a5b-4c6d-8e9f-0a1b2c3d4e5f",
        "channel_count": channel_count,
        "binding_digest": digest,
        "reconciled_at_ms": NOW - 3_000
    }))
    .unwrap()
}

fn worker(label: &str, stale: bool, runtime: Option<KeeperRuntimeObservationV1>) -> WorkerStatus {
    let sessions = runtime
        .as_ref()
        .map(|runtime| {
            (0..runtime.channel_count)
                .map(|index| format!("session-{index}"))
                .collect()
        })
        .unwrap_or_default();
    WorkerStatus {
        fingerprint: format!("{label}-fp"),
        label: label.to_string(),
        os: "linux".to_string(),
        reachable_addr: Some("100.64.0.7".to_string()),
        git_sha: Some("b1d1836a".to_string()),
        keeper_runtime: runtime,
        terminal_core_capacity: None,
        coordinator_open_session_ids: sessions,
        last_seen_ms: NOW - 1_000,
        age_ms: 1_000,
        stale,
    }
}

/// A target that is proven to be running nothing a staged release could destroy:
/// no definition at all.
fn target_with_no_service() -> TargetEvidence {
    TargetEvidence {
        service_observed: true,
        service_installed: false,
        service_state_observed: false,
        service_running: false,
        processes_observed: false,
        keeper_processes: 0,
        keeper_channel_processes: 0,
    }
}

/// A target whose definition is installed, whose service manager answered and
/// reported it stopped, and whose keeper processes hold no channel.
fn target_with_a_stopped_service(keeper_processes: u32, channel_processes: u32) -> TargetEvidence {
    TargetEvidence {
        service_observed: true,
        service_installed: true,
        service_state_observed: true,
        service_running: false,
        processes_observed: true,
        keeper_processes,
        keeper_channel_processes: channel_processes,
    }
}

/// The refusal a stale coordinator row raises, which is a claim about the
/// registry and not about the host.
const STALE_REFUSAL: &str = "existing Linux worker studio has a stale keeper update proof";

/// A stale registry row over a target that is running nothing stages, and prints
/// the evidence that allowed it. This is the machine `roost deploy` exists to
/// repair: refusing it on the coordinator's knowledge refuses exactly the host
/// that is down.
#[test]
fn a_stale_row_over_a_target_running_nothing_stages() {
    let staging = keeper_admission_staging(
        "Linux",
        AdmissionOutcome::ProofStale {
            worker_label: "studio".to_string(),
        },
    );
    assert!(staging.keeper_update.is_none());
    assert!(staging.worker_fingerprint.is_none());
    let refusal = staging
        .installed_service_refusal
        .expect("a stale row must raise a refusal");
    assert_eq!(refusal, STALE_REFUSAL);

    decide(&refusal, "studio", target_with_no_service())
        .expect("a target with no worker service is safe to stage onto");
}

/// The same row over a target whose service manager says the worker is RUNNING
/// still refuses. A stale row is not a licence: the target outranks it, and here
/// the target says a worker is alive and can prove its own admission.
#[test]
fn a_stale_row_over_a_running_worker_still_refuses() {
    let refusal = keeper_admission_staging(
        "Linux",
        AdmissionOutcome::ProofStale {
            worker_label: "studio".to_string(),
        },
    )
    .installed_service_refusal
    .expect("a stale row must raise a refusal");
    let evidence = TargetEvidence {
        service_running: true,
        ..target_with_a_stopped_service(1, 0)
    };
    let failure = decide(&refusal, "studio", evidence).expect_err("a running worker must refuse");
    assert_eq!(
        failure.code, 5,
        "keeper admission is the only code that means this"
    );
    assert!(
        failure
            .message
            .contains("is running and can prove admission"),
        "the refusal must name the evidence that decided it, got: {}",
        failure.message
    );
}

/// A keeper holding channels refuses even with the service stopped, because the
/// channels are what a staged release would destroy.
#[test]
fn a_keeper_holding_channels_refuses_even_with_the_worker_stopped() {
    let refusal = keeper_admission_staging("Linux", AdmissionOutcome::Unregistered)
        .installed_service_refusal
        .expect("an unregistered target must raise a refusal");
    let failure = decide(&refusal, "studio", target_with_a_stopped_service(1, 3))
        .expect_err("a keeper holding three channels must refuse");
    assert_eq!(failure.code, 5);
    assert!(
        failure.message.contains("still holds 3 channel process"),
        "the refusal must count the channels it would destroy, got: {}",
        failure.message
    );
}

/// A stopped worker whose keepers hold nothing stages. This is the documented
/// recovery path: the wedged keeper is gone, the service is not loaded, and the
/// operator is repairing the machine by deploying to it.
#[test]
fn a_stopped_worker_with_no_live_keeper_stages() {
    let refusal = keeper_admission_staging("Linux", AdmissionOutcome::Unregistered)
        .installed_service_refusal
        .expect("an unregistered target must raise a refusal");
    decide(&refusal, "studio", target_with_a_stopped_service(1, 0))
        .expect("a stopped worker holding no channel is safe to replace");
}

/// Every UNKNOWN refuses. An unreachable service manager reads exactly like a
/// stopped one in its own output, so treating "cannot tell" as "nothing is
/// running" is how a deploy replaces a machine that was holding somebody's
/// shells.
#[test]
fn an_unknown_never_stages() {
    let refusal = keeper_admission_staging("Linux", AdmissionOutcome::Unregistered)
        .installed_service_refusal
        .expect("an unregistered target must raise a refusal");

    let did_not_report = TargetEvidence {
        service_observed: false,
        ..target_with_no_service()
    };
    assert!(decide(&refusal, "studio", did_not_report).is_err());

    let manager_silent = TargetEvidence {
        service_state_observed: false,
        ..target_with_a_stopped_service(0, 0)
    };
    let failure = decide(&refusal, "studio", manager_silent).expect_err("a silent manager refuses");
    assert!(
        failure
            .message
            .contains("did not report the worker's state"),
        "got: {}",
        failure.message
    );

    // `pgrep` absent: the service questions were answered, the process counts were
    // not, and an unanswered process count is not evidence of an empty machine.
    let no_pgrep = TargetEvidence {
        processes_observed: false,
        ..target_with_a_stopped_service(0, 0)
    };
    let failure = decide(&refusal, "studio", no_pgrep).expect_err("a host without pgrep refuses");
    assert!(
        failure
            .message
            .contains("could not prove that no keeper is holding channels"),
        "got: {}",
        failure.message
    );
}

/// A keeper SOCKET FILE is not evidence. It outlives the keeper that created it,
/// so a probe that counted one would refuse a machine holding nothing and permit
/// one holding everything.
#[test]
fn a_keeper_socket_file_decides_nothing() {
    let command = target_worker_evidence_command(
        HostPlatform::Linux,
        "roost3-worker",
        "/home/studio/.config/systemd/user/roost3-worker.service",
    );
    assert!(
        !command.contains("test -e \"$keeper_socket\""),
        "the probe must not test the socket file: {command}"
    );
    let evidence = parse_target_evidence(
        0,
        "RoostServiceInstalled=yes\nRoostServiceState=observed\nActiveState=inactive\n\
         SubState=dead\nMainPID=0\nRoostKeeperProcesses=0\nRoostKeeperChannelProcesses=0\n\
         RoostTargetEvidence=complete\n",
        HostPlatform::Linux,
    );
    assert!(evidence.processes_observed);
    assert!(!evidence.service_running);
    assert_eq!(evidence.keeper_channel_processes, 0);
}

/// The probe is one command, and every value it interpolates is quoted: the
/// definition path comes from the target's environment and a path with a space in
/// it is a path a shell will otherwise split.
#[test]
fn the_evidence_probe_quotes_everything_it_interpolates() {
    let command = target_worker_evidence_command(
        HostPlatform::Linux,
        "roost3-worker",
        "/home/a b/.config/systemd/user/roost3-worker.service",
    );
    assert!(
        command.contains("'/home/a b/.config/systemd/user/roost3-worker.service'"),
        "the service spec must be single-quoted: {command}"
    );
    assert!(command.contains("'systemctl'"), "{command}");
    assert!(command.contains("'--user'"), "{command}");
    // The keeper's own socket name cannot match the command line that carries it,
    // which is why the leading character is bracketed.
    assert!(
        command.contains("[m]ux-keeper\\.sock"),
        "the keeper pattern must not match its own command line: {command}"
    );
    // `pgrep` is checked AFTER the checks that need no extra tooling, so a host
    // without it still reports what it can and is refused for the rest.
    let pgrep_check = command
        .find("command -v pgrep")
        .expect("the pgrep check must exist");
    let installed_check = command
        .find("RoostServiceInstalled=")
        .expect("the installed check must exist");
    assert!(
        installed_check < pgrep_check,
        "the installed check must not depend on pgrep"
    );
}

/// On darwin an unreachable launchd and an unloaded job share an exit code, so
/// the probe corroborates with a domain query that succeeds either way. On Linux
/// `systemctl show` already exits 0 for a unit it has never heard of, so there
/// is nothing to corroborate and nothing to add.
#[test]
fn the_darwin_probe_distinguishes_an_unreachable_launchd() {
    let command = target_worker_evidence_command(
        HostPlatform::MacOs,
        "com.roost.worker-v3",
        "Library/LaunchAgents/com.roost.worker-v3.plist",
    );
    assert!(
        command.contains("print-disabled"),
        "darwin must corroborate reachability: {command}"
    );
    let linux = target_worker_evidence_command(
        HostPlatform::Linux,
        "roost3-worker",
        "/x/roost3-worker.service",
    );
    assert!(
        !linux.contains("print-disabled"),
        "linux needs no corroboration: {linux}"
    );
}

/// A row whose build predates keeper-runtime reporting can never earn admission
/// for the update that teaches it to report, so it stages without a journaled
/// update and without a refusal. Demanding proof here would pin the worker on the
/// build that cannot produce it.
#[test]
fn a_worker_that_reports_no_keeper_runtime_stages_without_an_update() {
    let staging = keeper_admission_staging(
        "Linux",
        AdmissionOutcome::RuntimeUnreported {
            worker_label: "studio".to_string(),
        },
    );
    assert!(staging.keeper_update.is_none());
    assert!(
        staging.installed_service_refusal.is_none(),
        "an unreported runtime is the bootstrap allowance, not a refusal"
    );
}

/// A host is matched by any of the three names an operator might use for it, and
/// case does not matter — `roost deploy Studio` has to find the worker labelled
/// `studio`.
#[test]
fn a_host_matches_on_any_name_the_operator_might_use() {
    let row = worker("studio", false, None);
    assert!(worker_matches_target(&row, "studio"));
    assert!(worker_matches_target(&row, "Studio"));
    assert!(worker_matches_target(&row, "studio-fp"));
    assert!(worker_matches_target(&row, "100.64.0.7"));
    assert!(!worker_matches_target(&row, "mike-m5-air"));
}

/// The permitted verdict's own sentence names the evidence, because a permitted
/// staging is the one decision an operator will later want to justify.
#[test]
fn a_permitted_staging_says_what_allowed_it() {
    match installed_service_verdict(STALE_REFUSAL, "studio", target_with_a_stopped_service(2, 0)) {
        roost_cli::deploy::target_evidence::EvidenceVerdict::Permitted(evidence) => {
            assert!(evidence.contains("2 keeper process"), "got: {evidence}");
            assert!(evidence.contains("hold no channel"), "got: {evidence}");
        }
        roost_cli::deploy::target_evidence::EvidenceVerdict::Refused(refusal) => {
            panic!("a stopped worker holding no channel must stage, got: {refusal}")
        }
    }
}

/// A keeper that is holding nothing is the only thing a replace-empty may be
/// proved against, and the empty digest is a constant the protocol owns rather
/// than a value this test invents.
#[test]
fn the_empty_binding_digest_is_the_one_a_replace_may_be_proved_against() {
    let empty = keeper(0, EMPTY_DIGEST);
    assert!(empty.validate().is_ok());
    // Built field by field rather than through `parse`, because `parse` is what
    // refuses the contradiction — and the refusal is the thing under test.
    let contradicted = KeeperRuntimeObservationV1 {
        channel_count: 0,
        binding_digest: "c".repeat(64),
        ..empty
    };
    assert!(
        contradicted.validate().is_err(),
        "a channel count and a binding digest that disagree are evidence of nothing"
    );
}
