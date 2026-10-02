//! Pair requests and the control lane, decoded from the frames the coordinator
//! builds and applied through `ClientCore::handle`: the pair set and its
//! paired-browser notice, the `subscribed` and `domain_reset` barriers, UI
//! state and commands, relocation, and route/probe answers.
//!
//! Ports v2 `apps/web/tests/pairedBrowserNotice.test.ts` and the pair, UI and
//! control arms of `apps/web/src/store/sync-frame.ts` / `sync-inbound.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_decode_support;

use roost_client_core::SyncDomain;
use roost_client_core::store::sync_feeds::UI_COMMAND_QUEUE_MAX;
use roost_client_core::sync::decode::DecodeRefusal;
use roost_client_core::sync::inbound::PairedBrowser;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::pair_request_delta_proto::Kind as PairKind;
use roost_proto::{
    CoordinatorRelocationFrame, PairCompleted, PairRequest, PairRequestDeltaProto,
    PairRequestsSnapshot, SyncDomainResetFrame, TerminalInputRouteResult,
    TerminalTransportProbeResult, UiCommandFrame, UiStateFrame,
};

use sync_decode_support::{
    SESSION, SOCKET, WORKER_FP, acked, application, closes, control, deliver, ready_core, refused,
    subscribed_arm,
};

fn pair_arm(kind: PairKind) -> Frame {
    Frame::PairRequestDelta(Box::new(PairRequestDeltaProto {
        kind: Some(kind),
        ..PairRequestDeltaProto::default()
    }))
}

fn pending(ephemeral_id: &str) -> PairRequest {
    PairRequest {
        ephemeral_id: ephemeral_id.to_owned(),
        label: "Chrome — macOS".to_owned(),
        created_at_ms: 1,
        expires_at_ms: 60_000,
        ..PairRequest::default()
    }
}

fn pair_ids(core: &roost_client_core::ClientCore) -> Vec<&str> {
    core.store()
        .pair_requests
        .keys()
        .map(String::as_str)
        .collect()
}

#[test]
fn a_pair_snapshot_replaces_the_set_so_a_missed_removal_cannot_linger() {
    let (mut core, generation) = ready_core();
    for (seq, id) in [(1, "a"), (2, "b")] {
        let arm = pair_arm(PairKind::Pending(Box::new(pending(id))));
        deliver(
            &mut core,
            generation,
            &application(SyncDomain::Pair, seq, arm),
        );
    }
    let removed = pair_arm(PairKind::RemovedId("a".to_owned()));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Pair, 3, removed),
    );
    assert_eq!(pair_ids(&core), ["b"]);

    let snapshot = pair_arm(PairKind::Snapshot(Box::new(PairRequestsSnapshot {
        pending: vec![pending("c"), pending("d")],
        ..PairRequestsSnapshot::default()
    })));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Pair, 4, snapshot),
    );
    assert_eq!(pair_ids(&core), ["c", "d"], "b was not in the snapshot");
}

fn completed(ephemeral_id: &str) -> Frame {
    pair_arm(PairKind::Completed(Box::new(PairCompleted {
        ephemeral_id: ephemeral_id.to_owned(),
        label: "Chrome — macOS".to_owned(),
        client_browser: "Chrome".to_owned(),
        client_os: "macOS".to_owned(),
        city: "Berlin".to_owned(),
        region: "Berlin".to_owned(),
        country_code: "DE".to_owned(),
        paired_at_ms: 1,
        ..PairCompleted::default()
    })))
}

fn toast_messages(core: &roost_client_core::ClientCore) -> Vec<String> {
    core.store()
        .toasts
        .toasts()
        .map(|toast| toast.msg.clone())
        .collect()
}

#[test]
fn a_completed_pairing_drops_the_card_and_announces_the_new_browser_once() {
    // v2 pairedBrowserNotice.test.ts "drops the approved card and announces the
    // new browser once".
    let (mut core, generation) = ready_core();
    let id = "c".repeat(32);
    let arm = pair_arm(PairKind::Pending(Box::new(pending(&id))));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Pair, 1, arm),
    );
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Pair, 2, completed(&id)),
    );
    for toast in core
        .store()
        .toasts
        .toasts()
        .map(|toast| toast.id.clone())
        .collect::<Vec<_>>()
    {
        roost_client_core::store::toasts::dismiss_toast(core.store_mut(), &toast);
    }
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Pair, 3, completed(&id)),
    );
    assert!(core.store().pair_requests.is_empty());
    assert!(
        toast_messages(&core).is_empty(),
        "a second report of the same pairing raises no second card"
    );
    let other = "d".repeat(32);
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Pair, 4, completed(&other)),
    );
    assert_eq!(
        toast_messages(&core),
        ["New browser paired: Chrome on macOS · Berlin"]
    );
}

fn described(browser: &str, os: &str, city: &str, region: &str, country: &str) -> PairedBrowser {
    PairedBrowser {
        ephemeral_id: String::new(),
        label: "Chrome — macOS".to_owned(),
        client_browser: browser.to_owned(),
        client_os: os.to_owned(),
        city: city.to_owned(),
        region: region.to_owned(),
        country_code: country.to_owned(),
    }
}

#[test]
fn the_notice_names_browser_and_os_then_the_most_specific_place() {
    // v2 pairedBrowserNotice.test.ts `formatPairedBrowserLabel`, both cases.
    let label = |browser: PairedBrowser| browser.announcement_label();
    assert_eq!(
        label(described("Chrome", "macOS", "Berlin", "Berlin", "DE")),
        "Chrome on macOS · Berlin"
    );
    assert_eq!(
        label(described("Chrome", "macOS", " ", "", "DE")),
        "Chrome on macOS · DE"
    );
    assert_eq!(
        label(described("Chrome", "macOS", "", "", "")),
        "Chrome on macOS"
    );
    assert_eq!(
        label(described("", "", "", "Berlin", "DE")),
        "Chrome — macOS · Berlin"
    );
    assert_eq!(
        label(described("Chrome", "", "Berlin", "", "")),
        "Chrome · Berlin"
    );
}

#[test]
fn a_malformed_subscribed_or_domain_reset_is_refused() {
    // v2 handleSubscribed / handleDomainReset throw, which closes the link.
    let mut incomplete = subscribed_arm(SOCKET);
    if let Frame::Subscribed(value) = &mut incomplete {
        value.generations.pop();
    }
    let no_socket = Frame::Subscribed(Box::default());
    let zero_generation = Frame::DomainReset(Box::new(SyncDomainResetFrame {
        domain: roost_proto::SyncDomain::Workers.into(),
        generation: 0,
        ..SyncDomainResetFrame::default()
    }));
    let unspecified_domain = Frame::DomainReset(Box::new(SyncDomainResetFrame {
        generation: 4,
        ..SyncDomainResetFrame::default()
    }));
    for (arm, name) in [
        (incomplete, "subscribed"),
        (no_socket, "subscribed"),
        (zero_generation, "domain_reset"),
        (unspecified_domain, "domain_reset"),
    ] {
        let refusal = refused(&control(arm));
        assert!(
            matches!(&refusal, DecodeRefusal::MalformedArm { arm, .. } if *arm == name),
            "{refusal}"
        );
    }
}

#[test]
fn peer_ui_state_is_consumed_and_changes_nothing() {
    let (mut core, generation) = ready_core();
    let revision = core.store().revision();
    let state = Frame::UiState(Box::new(UiStateFrame {
        fp: WORKER_FP.to_owned(),
        tab_id: "other-tab".to_owned(),
        ..UiStateFrame::default()
    }));
    let effects = deliver(&mut core, generation, &control(state));
    assert!(effects.is_empty(), "{effects:?}");
    assert_eq!(core.store().revision(), revision);
}

#[test]
fn ui_commands_queue_for_the_bridge_oldest_dropped_past_the_bound() {
    let (mut core, generation) = ready_core();
    for index in 0..=UI_COMMAND_QUEUE_MAX {
        let command = Frame::UiCommand(Box::new(UiCommandFrame {
            target_tab_id: format!("tab-{index}"),
            ..UiCommandFrame::default()
        }));
        deliver(&mut core, generation, &control(command));
    }
    let queued = &core.store().ui_commands;
    assert_eq!(queued.len(), UI_COMMAND_QUEUE_MAX);
    assert_eq!(
        queued.front().map(|frame| frame.target_tab_id.as_str()),
        Some("tab-1")
    );
    let newest = format!("tab-{UI_COMMAND_QUEUE_MAX}");
    assert_eq!(
        queued.back().map(|frame| frame.target_tab_id.as_str()),
        Some(newest.as_str())
    );
}

#[test]
fn a_relocation_notice_closes_the_link_as_an_unknown_v2_control() {
    let (mut core, generation) = ready_core();
    let relocation = Frame::CoordinatorRelocation(Box::new(CoordinatorRelocationFrame {
        handoff_id: "handoff-1".to_owned(),
        source_url: "https://old.example".to_owned(),
        target_url: "https://new.example".to_owned(),
        ..CoordinatorRelocationFrame::default()
    }));
    let effects = deliver(&mut core, generation, &control(relocation));
    assert!(closes(&effects, generation), "{effects:?}");
    assert!(!core.store().sync.accepts(generation));
}

#[test]
fn an_unsolicited_route_answer_installs_no_epoch_and_a_probe_answer_is_held() {
    let (mut core, generation) = ready_core();
    let route = Frame::InputRouteResult(Box::new(TerminalInputRouteResult {
        request_id: "claim-1".to_owned(),
        session_id: SESSION.to_owned(),
        revision: 2,
        accepted: true,
        input_route_epoch: "route-epoch".to_owned(),
        worker_epoch: "worker-epoch".to_owned(),
        ..TerminalInputRouteResult::default()
    }));
    deliver(&mut core, generation, &control(route));
    // An answer to a claim this document never sent is somebody else's route:
    // installing its epoch would stamp this tab's input with it.
    assert!(
        core.store()
            .input
            .lane(SESSION)
            .is_none_or(|lane| lane.route_epoch.is_empty()),
        "an unsolicited route answer installed an epoch"
    );

    let probe = |worker_epoch: &str| {
        Frame::TerminalTransportProbeResult(Box::new(TerminalTransportProbeResult {
            request_id: "probe-1".to_owned(),
            worker_fp: WORKER_FP.to_owned(),
            worker_epoch: worker_epoch.to_owned(),
            ..TerminalTransportProbeResult::default()
        }))
    };
    // An empty epoch is the coordinator's refusal and never becomes telemetry.
    let effects = deliver(&mut core, generation, &control(probe("")));
    assert!(
        acked(&effects).is_empty(),
        "a control is never acknowledged"
    );
    assert!(core.store().transport_probes.is_empty());
    deliver(&mut core, generation, &control(probe("worker-epoch")));
    let sample = &core.store().transport_probes[WORKER_FP];
    assert_eq!(
        (sample.worker_epoch.as_str(), sample.socket_generation),
        ("worker-epoch", generation)
    );
}
