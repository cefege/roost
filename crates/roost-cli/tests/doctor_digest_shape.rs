//! `roost doctor`'s digest: the classification rules, the exact rendered text,
//! and the exit code, all from synthetic log lines. Nothing here touches a real
//! log file, a service, or a network — the digest is a pure function of the
//! lines it is fed, and a documented output can only be pinned if the function
//! that produces it can be called without a machine.
//!
//! Ported from `apps/roost-cli/tests/doctor.test.ts`, which was the executable
//! spec of the classification contract, plus a whole-document golden: the
//! per-assertion tests say the rules are right, and the golden says the page an
//! operator reads has not moved.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_cli::doctor::audit::AuditSummary;
use roost_cli::doctor::digest::{Digest, classify};
use roost_cli::doctor::digest_render::render_digest;
use serde_json::{Value, json};

/// 2026-06-19T20:13:00Z. Fixed, so the golden's timestamps are a fact about the
/// fixture rather than about the day the test runs.
const NOW: i64 = 1_781_900_000_000;
const CUTOFF: i64 = NOW - 24 * 3_600_000;

fn fold(lines: &[Value]) -> Digest {
    let mut digest = Digest::new();
    for line in lines {
        classify(&mut digest, line, CUTOFF, "coord");
    }
    digest
}

fn render(digest: &Digest) -> roost_cli::doctor::digest_render::DigestOutput {
    render_digest(
        digest,
        "24h",
        CUTOFF,
        &[],
        &AuditSummary::default(),
        "testhost",
    )
}

fn signal(kind: &str) -> Value {
    json!({"ts": NOW, "level": "warn", "target": "signal", "msg": kind, "src": "spa"})
}

#[test]
fn groups_signals_by_kind_rolls_up_sub_kinds_and_counts_sessions() {
    let digest = fold(&[
        json!({"ts": NOW, "level": "warn", "target": "signal", "msg": "diag.corruption_signal", "kind": "resize_storm", "sid": "s1", "src": "spa"}),
        json!({"ts": NOW, "level": "warn", "target": "signal", "msg": "diag.corruption_signal", "kind": "resize_storm", "sid": "s2", "src": "spa"}),
        json!({"ts": NOW, "level": "warn", "target": "signal", "msg": "diag.corruption_signal", "kind": "suppress_imbalance", "sid": "s1", "src": "spa"}),
        json!({"ts": NOW, "level": "warn", "target": "signal", "msg": "auth.relogin_401", "sid": "s1", "src": "spa"}),
    ]);
    let corruption = digest.signals.get("diag.corruption_signal").unwrap();
    assert_eq!(corruption.count, 3);
    assert_eq!(corruption.session_ids.len(), 2);
    assert_eq!(corruption.sub_kinds.get("resize_storm"), Some(&2));
    assert_eq!(corruption.sub_kinds.get("suppress_imbalance"), Some(&1));
    assert_eq!(digest.signals.get("auth.relogin_401").unwrap().count, 1);
}

#[test]
fn infra_warnings_split_out_from_signals_and_count_by_target_and_message() {
    let digest = fold(&[
        json!({"ts": NOW, "level": "warn", "target": "worker-service", "msg": "reader_failed"}),
        json!({"ts": NOW, "level": "warn", "target": "worker-service", "msg": "reader_failed"}),
        json!({"ts": NOW, "level": "warn", "target": "pending-rpcs", "msg": "timeout"}),
        signal("spa.uncaught"),
    ]);
    assert_eq!(digest.infra.get("worker-service / reader_failed"), Some(&2));
    assert_eq!(digest.infra.get("pending-rpcs / timeout"), Some(&1));
    assert_eq!(
        digest.signals.len(),
        1,
        "a signal must not also count as infra"
    );
}

#[test]
fn lines_older_than_the_cutoff_and_below_warn_are_dropped() {
    let digest = fold(&[
        json!({"ts": CUTOFF - 1, "level": "warn", "target": "signal", "msg": "spa.uncaught"}),
        json!({"ts": NOW, "level": "info", "target": "diag", "msg": "viewport.claim"}),
        signal("spa.uncaught"),
    ]);
    assert_eq!(digest.total_signals(), 1);
}

#[test]
fn any_signal_or_error_level_line_exits_one_and_routine_infra_exits_zero() {
    assert_eq!(render(&fold(&[signal("input.drop_burst")])).exit_code, 1);
    assert_eq!(
        render(&fold(&[
            json!({"ts": NOW, "level": "warn", "target": "worker-service", "msg": "reader_failed"})
        ]))
        .exit_code,
        0
    );
    // An error-level line trips the gate even with no signal at all.
    assert_eq!(
        render(&fold(&[
            json!({"ts": NOW, "level": "error", "target": "coord", "msg": "boom"})
        ]))
        .exit_code,
        1
    );
}

#[test]
fn an_operator_caused_capture_lifecycle_is_listed_but_does_not_trip_the_exit() {
    let lifecycle = fold(&[
        signal("terminal.capture_started"),
        signal("terminal.capture_saved"),
        signal("terminal.capture_stopped"),
    ]);
    let output = render(&lifecycle);
    assert_eq!(output.exit_code, 0);
    assert!(output.text.contains("terminal.capture_saved"));

    // The capture FAULT kinds are the anomalies the feature exists to surface.
    for fault in [
        "terminal.capture_failed",
        "terminal.history_conflict",
        "terminal.emission_conflict",
    ] {
        let digest = fold(&[signal("terminal.capture_started"), signal(fault)]);
        assert_eq!(
            render(&digest).exit_code,
            1,
            "{fault} did not trip the gate"
        );
    }
}

#[test]
fn a_structured_message_never_splits_one_kind_into_two_groups() {
    // Both lines carry a different human message; grouping by message would
    // produce two groups of one and hide the kind entirely.
    let digest = fold(&[
        json!({"ts": NOW, "level": "warn", "target": "signal", "evt": "spa.uncaught", "msg": "Cannot read x of null", "kind": "error", "sid": "s1", "src": "spa"}),
        json!({"ts": NOW, "level": "warn", "target": "signal", "evt": "spa.uncaught", "msg": "totally different error text", "kind": "error", "sid": "s2", "src": "spa"}),
    ]);
    assert_eq!(digest.signals.len(), 1);
    assert_eq!(digest.signals.get("spa.uncaught").unwrap().count, 2);
}

#[test]
fn error_kinds_wear_the_red_icon_and_the_rest_wear_the_warning() {
    let output = render(&fold(&[
        signal("event.append_failed"),
        signal("rpc.worker_timeout"),
        signal("deploy.failed"),
        signal("sync.queue_overflow"),
    ]));
    assert_eq!(output.exit_code, 1);
    let line = |kind: &str| {
        output
            .text
            .lines()
            .find(|line| line.contains(kind))
            .unwrap_or_default()
            .to_string()
    };
    assert!(line("event.append_failed").contains('\u{1F534}'));
    assert!(line("deploy.failed").contains('\u{1F534}'));
    assert!(line("rpc.worker_timeout").contains('\u{26A0}'));
    assert!(line("sync.queue_overflow").contains('\u{26A0}'));
}

#[test]
fn a_keeper_subprocess_signal_is_attributed_to_the_keeper() {
    let mut digest = Digest::new();
    classify(
        &mut digest,
        &json!({"ts": NOW, "level": "warn", "target": "signal", "msg": "keeper.died", "kind": "adopted_socket_close"}),
        CUTOFF,
        "keeper",
    );
    assert!(
        digest
            .signals
            .get("keeper.died")
            .unwrap()
            .sources
            .contains("keeper")
    );
    assert!(render(&digest).text.contains("src:keeper"));
}

/// The whole page, byte for byte. Everything above says the rules are right;
/// this says the document an operator pastes into a ticket has not moved.
#[test]
fn the_whole_digest_is_the_documented_text() {
    let digest = fold(&[
        json!({"ts": NOW, "level": "warn", "target": "signal", "msg": "diag.corruption_signal", "kind": "resize_storm", "sid": "s1", "src": "spa"}),
        json!({"ts": NOW, "level": "warn", "target": "signal", "msg": "diag.corruption_signal", "kind": "resize_storm", "sid": "s1", "src": "spa"}),
        json!({"ts": NOW, "level": "warn", "target": "worker-service", "msg": "reader_failed"}),
    ]);
    let expected = "\
# roost doctor — last 24h
host=testhost  sources=coord+worker(local)  cutoff=2026-06-18 20:13

## signals (always-on anomaly channel)
  ⚠️  diag.corruption_signal     2  resize_storm×2  (1 sid, src:spa)

## infra warnings (reconnect / rate-limit / external)
     1  worker-service / reader_failed

## audit (coordinator request log)
  ✓ no failed calls in window

## summary
  window:  2026-06-19 20:13 → 2026-06-19 20:13
  signals: 2 (1 kinds)   infra: 1   errors: 0
  health:  run `roost status` (services / coord / public url / workers)
  exit:    1 (review above)";
    assert_eq!(render(&digest).text, expected);
}

#[test]
fn an_empty_window_says_so_rather_than_printing_a_bare_heading() {
    let output = render(&Digest::new());
    assert_eq!(output.exit_code, 0);
    assert!(output.text.contains("  ✓ none in window"));
    assert!(output.text.contains("  window:  (no events)"));
    assert!(output.text.contains("  exit:    0 (nothing to review)"));
}
