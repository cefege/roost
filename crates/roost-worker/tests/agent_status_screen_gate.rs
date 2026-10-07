//! Screen-read gate: back-to-back status scans inside `SCREEN_RESCAN_MIN_MS`
//! read each session's visible grid once, the grid is re-read after the
//! window, and a vanished or closed session's gate entry is dropped so its
//! return re-reads immediately. Ports v2
//! `apps/worker/tests/agents/agent-status-screen-gate.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_status_support;

use std::sync::atomic::Ordering;
use std::time::Duration;

use agent_status_support::{OTHER_SESSION_ID, SESSION_ID, detector_harness, session};
use roost_worker::agents::BuiltinAgentId as Agent;

#[tokio::test]
async fn back_to_back_scans_inside_the_window_read_the_visible_grid_once() {
    let harness = detector_harness();
    harness.scanner.set(SESSION_ID, Agent::Omp, 4_321);
    harness.sessions.add("");
    let reads = || harness.sessions.reads.load(Ordering::SeqCst);
    harness.detector.scan_now().await;
    assert_eq!(reads(), 1);
    harness.clock.advance(150);
    harness.detector.scan_now().await;
    assert_eq!(reads(), 1);
    harness.clock.advance(49);
    harness.detector.scan_now().await;
    assert_eq!(reads(), 1);
    // Exactly 200 ms elapsed is not "newer than" the minimum: re-read.
    harness.clock.advance(1);
    harness.detector.scan_now().await;
    assert_eq!(reads(), 2);
    harness.detector.dispose();
}

#[tokio::test]
async fn same_kind_process_replacement_reaches_the_registry_with_the_verified_pid() {
    let harness = detector_harness();
    harness.scanner.set(SESSION_ID, Agent::Omp, 4_321);
    harness.sessions.add("");
    // Two scans per identity: the acquisition grace window withholds the first
    // evaluation of a newly detected process.
    harness.detector.scan_now().await;
    harness.clock.advance(200);
    harness.detector.scan_now().await;
    let first = harness.published.last();

    harness.scanner.set(SESSION_ID, Agent::Omp, 4_322);
    harness.clock.advance(200);
    harness.detector.scan_now().await;
    harness.clock.advance(200);
    harness.detector.scan_now().await;

    let (retired, replacement) = (harness.published.nth_last(2), harness.published.nth_last(1));
    assert!(!retired.active);
    assert!(replacement.active);
    assert_eq!(retired.common.occupant_id, first.common.occupant_id);
    assert_ne!(replacement.common.occupant_id, first.common.occupant_id);
    assert_eq!(replacement.common.status_epoch, first.common.status_epoch);
    harness.detector.dispose();
}

#[tokio::test]
async fn a_vanished_sessions_gate_entry_is_pruned_so_its_return_re_reads_immediately() {
    let harness = detector_harness();
    harness.scanner.set(SESSION_ID, Agent::Omp, 4_321);
    harness.sessions.add("");
    harness.detector.scan_now().await;
    assert_eq!(harness.sessions.reads.load(Ordering::SeqCst), 1);
    harness.sessions.clear();
    harness.detector.scan_now().await;
    harness.sessions.add("");
    harness.clock.advance(10);
    harness.detector.scan_now().await;
    assert_eq!(harness.sessions.reads.load(Ordering::SeqCst), 2);
    harness.detector.dispose();
}

#[tokio::test]
async fn close_session_drops_the_gate_entry_so_a_reused_session_id_re_reads() {
    let harness = detector_harness();
    harness.scanner.set(SESSION_ID, Agent::Omp, 4_321);
    harness.sessions.add("");
    harness.detector.scan_now().await;
    assert_eq!(harness.sessions.reads.load(Ordering::SeqCst), 1);
    harness.detector.close_session(&session(SESSION_ID));
    harness.clock.advance(10);
    harness.detector.scan_now().await;
    assert_eq!(harness.sessions.reads.load(Ordering::SeqCst), 2);
    harness.detector.dispose();
}

#[tokio::test]
async fn a_coalesce_timer_armed_for_a_closing_session_is_cancelled_not_fired() {
    let harness = detector_harness();
    harness.scanner.set(SESSION_ID, Agent::Omp, 4_321);
    harness.sessions.add("");
    harness.detector.scan_now().await;
    let scans_before_close = harness.scanner.scans.load(Ordering::SeqCst);
    harness.detector.schedule(7);
    harness.detector.close_session(&session(SESSION_ID));
    // Well past the 40 ms coalesce delay: a timer that fired would have scanned.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        harness.scanner.scans.load(Ordering::SeqCst),
        scans_before_close
    );
    harness.detector.dispose();
}

#[tokio::test]
async fn close_session_evicts_the_sessions_cached_report_capability() {
    let harness = detector_harness();
    harness.environment.session_overlay(SESSION_ID).unwrap();
    harness
        .environment
        .session_overlay(OTHER_SESSION_ID)
        .unwrap();
    harness.detector.close_session(&session(SESSION_ID));
    // The closed session's entry is already gone; the other one survives.
    assert_eq!(
        harness
            .environment
            .release_agent_status_capabilities(&session(SESSION_ID)),
        0
    );
    assert_eq!(
        harness
            .environment
            .release_agent_status_capabilities(&session(OTHER_SESSION_ID)),
        1
    );
    harness.detector.dispose();
}
