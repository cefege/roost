//! `roost status`'s rendered readout: the exact text, the worker row's update
//! position, and what the exit code is allowed to depend on. Everything runs
//! from a fixture with no service manager, no socket, and no database, because
//! the readout is a pure function of a report and that is the only way a
//! documented shape can be pinned.
//!
//! The worker-row cases are ported from `apps/roost-cli/tests/status-output.test.ts`,
//! which was the executable spec of that row. The whole-screen golden is new:
//! the per-assertion cases say the row is right, and the golden says the screen
//! an operator reads has not moved.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_cli::status::render::{ServiceLabels, render_status_report, status_report_is_healthy};
use roost_cli::status::report::{
    CoordStatus, EndpointStatus, SpaStatus, StatusReport, WorkerStatus,
};
use roost_protocol::keeper_update::KeeperRuntimeObservationV1;
use roost_protocol::wire::TerminalCoreCapacityReport;
use serde_json::json;

const COORD_SHA: &str = "b1d1836a9f4c2e1d7a0b5c6d8e9f0a1b2c3d4e5f";
const BEHIND_SHA: &str = "0fa77c31de0e4b5a6c7d8e9f0a1b2c3d4e5f6071";
/// 2026-06-19T20:13:00Z. Fixed, so an age in the golden is a fact about the
/// fixture rather than about the day the test runs.
const NOW: i64 = 1_781_900_000_000;
const LABELS: ServiceLabels<'static> = ServiceLabels {
    coord: "roost3-coord",
    worker: "roost3-worker",
};
const FRONT_DOOR: &str = "https://dash.example.test";
const SERVED_DIST: &str = "/repo/apps/web/dist";

/// The healthy install the golden is taken from: both services loaded, the
/// coordinator answering, a front door that answers, a served SPA, no workers.
fn healthy_install() -> StatusReport {
    StatusReport {
        coord_agent_loaded: true,
        worker_agent_loaded: true,
        coord: CoordStatus {
            reachable: true,
            git_sha: None,
        },
        workers: Vec::new(),
        endpoint: EndpointStatus {
            public_url: Some(FRONT_DOOR.to_string()),
            answers: true,
        },
        spa: SpaStatus {
            serves: Some(true),
            web_dist_path: Some(SERVED_DIST.to_string()),
            web_dist_present: true,
        },
    }
}

fn with_workers(workers: Vec<WorkerStatus>) -> StatusReport {
    StatusReport {
        workers,
        ..healthy_install()
    }
}

fn with_coord(sha: Option<&str>, workers: Vec<WorkerStatus>) -> StatusReport {
    StatusReport {
        coord: CoordStatus {
            reachable: true,
            git_sha: sha.map(str::to_string),
        },
        workers,
        ..healthy_install()
    }
}

#[allow(clippy::too_many_arguments)]
fn worker(label: &str, git_sha: Option<&str>, age_ms: i64, stale: bool) -> WorkerStatus {
    WorkerStatus {
        fingerprint: format!("{label}-fp"),
        label: label.to_string(),
        os: "darwin".to_string(),
        reachable_addr: Some("100.64.0.7".to_string()),
        git_sha: git_sha.map(str::to_string),
        keeper_runtime: None,
        terminal_core_capacity: None,
        coordinator_open_session_ids: Vec::new(),
        last_seen_ms: NOW - age_ms,
        age_ms,
        stale,
    }
}

fn render(report: &StatusReport) -> String {
    render_status_report(report, LABELS, NOW)
}

fn line_containing(report: &StatusReport, needle: &str) -> String {
    render(report)
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_default()
        .to_string()
}

#[test]
fn a_worker_on_the_coordinators_sha_is_up_to_date() {
    let report = with_coord(
        Some(COORD_SHA),
        vec![worker("mike-m5-air", Some(COORD_SHA), 10_000, false)],
    );
    assert_eq!(
        line_containing(&report, "mike-m5-air"),
        "    ✓ mike-m5-air — last seen 10s ago · b1d1836a · Up to date"
    );
}

#[test]
fn a_reachable_worker_behind_the_coordinator_has_an_update_available() {
    let report = with_coord(
        Some(COORD_SHA),
        vec![worker("mike-m5-air", Some(BEHIND_SHA), 10_000, false)],
    );
    assert_eq!(
        line_containing(&report, "mike-m5-air"),
        "    ✓ mike-m5-air — last seen 10s ago · 0fa77c31 · Update available"
    );
}

#[test]
fn a_stale_worker_behind_the_coordinator_is_deferred_and_the_install_is_still_healthy() {
    let report = with_coord(
        Some(COORD_SHA),
        vec![worker("m1-us", Some(BEHIND_SHA), 3_600_000, true)],
    );
    assert_eq!(
        line_containing(&report, "m1-us"),
        "    ✗ m1-us — last seen 3600s ago (STALE) · 0fa77c31 · Update pending — offline"
    );
    // The rows are not part of the exit code: a sleeping laptop is deferred,
    // not broken, and a gate that turned red every night would be ignored.
    assert!(status_report_is_healthy(&report));
}

#[test]
fn a_worker_that_reported_no_sha_prints_the_unknown_label_and_no_sha() {
    let report = with_coord(
        Some(COORD_SHA),
        vec![worker("mike-m5-air", None, 10_000, false)],
    );
    assert_eq!(
        line_containing(&report, "mike-m5-air"),
        "    ✓ mike-m5-air — last seen 10s ago · Version unknown"
    );
}

#[test]
fn an_unreachable_coordinator_cannot_classify_any_worker() {
    let report = StatusReport {
        coord: CoordStatus {
            reachable: false,
            git_sha: None,
        },
        workers: vec![worker("mike-m5-air", Some(COORD_SHA), 10_000, false)],
        ..healthy_install()
    };
    assert_eq!(
        line_containing(&report, "mike-m5-air"),
        "    ✓ mike-m5-air — last seen 10s ago · b1d1836a · Version unknown"
    );
}

/// The whole screen, byte for byte, for the install an operator sees on a
/// healthy machine with a front door and no workers registered yet.
#[test]
fn the_whole_readout_is_the_documented_text() {
    let expected = "\
roost status
  ✓ coordinator service (roost3-coord)
  ✓ worker service (roost3-worker)
  ✓ coord reachable
  ✓ public url https://dash.example.test
  ✓ spa: served (/repo/apps/web/dist)
  ✗ workers: none registered
  open: https://dash.example.test";
    assert_eq!(render(&healthy_install()), expected);
}

#[test]
fn an_undeclared_front_door_prints_as_local_only_and_passes_the_gate() {
    let report = StatusReport {
        endpoint: EndpointStatus {
            public_url: None,
            answers: false,
        },
        ..healthy_install()
    };
    let text = render(&report);
    assert!(text.contains("  ✓ public url: local-only access"));
    assert!(!text.contains("  open:"));
    assert!(status_report_is_healthy(&report));
}

#[test]
fn a_declared_front_door_that_does_not_answer_fails_the_gate_and_names_the_remedy() {
    let report = StatusReport {
        endpoint: EndpointStatus {
            public_url: Some(FRONT_DOOR.to_string()),
            answers: false,
        },
        ..healthy_install()
    };
    let text = render(&report);
    assert!(text.contains("  ✗ public url https://dash.example.test"));
    assert!(
        text.contains("      → that URL does not answer AuthCoordIdentity; point your front door")
    );
    assert!(!status_report_is_healthy(&report));
}

#[test]
fn a_404_root_names_which_of_its_three_causes_applies() {
    let cases = [
        (
            false,
            "  ✗ spa: MISSING (ROOST_WEB_DIST_PATH=/srv/web has no index.html)",
        ),
        (
            true,
            "  ✗ spa: MISSING (ROOST_WEB_DIST_PATH=/srv/web exists but the coordinator serves no page)",
        ),
        (
            false,
            "  ✗ spa: MISSING (no ROOST_WEB_DIST_PATH and no embedded build)",
        ),
    ];
    for (present, expected) in cases {
        let report = StatusReport {
            spa: SpaStatus {
                serves: Some(false),
                web_dist_path: (!expected.contains("no ROOST_WEB_DIST_PATH"))
                    .then(|| "/srv/web".to_string()),
                web_dist_present: present,
            },
            ..healthy_install()
        };
        assert!(render(&report).contains(expected), "missing: {expected}");
    }
}

#[test]
fn an_unprobed_spa_is_neither_a_pass_nor_a_failure() {
    let report = StatusReport {
        spa: SpaStatus {
            serves: None,
            web_dist_path: None,
            web_dist_present: false,
        },
        ..healthy_install()
    };
    assert!(render(&report).contains("  - spa: not probed (no coordinator listener on this host)"));
}

#[test]
fn a_keeper_runtime_and_a_capacity_report_print_on_their_own_lines() {
    // Both fixtures go through the protocol's own validators, so a readout that
    // renders them is rendering values the wire accepts — not a shape this test
    // invented.
    let keeper = KeeperRuntimeObservationV1::parse(&json!({
        "schema_version": 1,
        "running_contract": {
            "protocol_version": 3,
            "supported_features": [],
            "required_features": [],
            "implementation_digest": "a".repeat(64),
            "platform": "linux",
            "arch": "x86_64",
            "build_sha": "b1d1836a"
        },
        "keeper_pid": 4242,
        "keeper_epoch": "6f1c2d3e-4a5b-4c6d-8e9f-0a1b2c3d4e5f",
        "channel_count": 2,
        "binding_digest": "c".repeat(64),
        "reconciled_at_ms": NOW - 3_000
    }))
    .unwrap();
    let capacity = TerminalCoreCapacityReport::parse(json!({
        "used": 1,
        "pending": 0,
        "capacity": 4,
        "estimated_reserved_bytes": 12 * 1024 * 1024,
        "effective_memory_ceiling_bytes": 1024 * 1024 * 1024,
        "boot_rss_bytes": 64 * 1024 * 1024,
        "overcommit_count": 0,
        "refusal_count": 0
    }))
    .unwrap();
    let report = with_workers(vec![WorkerStatus {
        keeper_runtime: Some(keeper),
        terminal_core_capacity: Some(capacity),
        ..worker("mike-m5-air", Some(COORD_SHA), 10_000, false)
    }]);
    let text = render(&report);
    assert!(text.contains(
        "      keeper: pid 4242, epoch 6f1c2d3e-4a5b-4c6d-8e9f-0a1b2c3d4e5f, 2 channel(s), \
         bindings cccccccccccc, reconciled 3s ago"
    ));
    assert!(
        text.contains("      terminal cores: 1/4 resident, 0 pending, 12 MiB reserved, 0 refused")
    );
}

#[test]
fn a_worker_with_no_keeper_observation_says_admission_is_unproven() {
    let text = render(&with_workers(vec![worker(
        "mike-m5-air",
        Some(COORD_SHA),
        10_000,
        false,
    )]));
    assert!(text.contains("      keeper: update admission unproven"));
    assert!(text.contains("      terminal cores: capacity unavailable"));
}

#[test]
fn a_machine_with_neither_service_loaded_names_both_remedies() {
    let report = StatusReport {
        coord_agent_loaded: false,
        worker_agent_loaded: false,
        ..healthy_install()
    };
    let text = render(&report);
    assert!(text.contains("  ✗ coordinator service (roost3-coord)"));
    assert!(text.contains("      → roost quickstart"));
    assert!(text.contains("  ✗ worker service (roost3-worker)"));
    assert!(text.contains("      → roost deploy localhost"));
    assert!(!status_report_is_healthy(&report));
}
