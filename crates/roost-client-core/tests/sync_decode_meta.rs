//! The Sync decoder's refusals and the arm table: bytes that are not a frame,
//! the v2 meta rule, a refused frame closing exactly the link that sent it, and
//! the proto-divergence guard that makes a new `FirehoseFrame` arm fail here.
//!
//! Ported from v2 `apps/web/src/store/sync-inbound.ts` (`handleV2Control`'s
//! "sequenced v2 control", `dispatchV2Application`'s "malformed v2 application
//! frame", `_consumeSyncFrame`'s close path).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_decode_support;

use std::collections::BTreeMap;
use std::path::PathBuf;

use roost_client_core::effect::Effect;
use roost_client_core::event::ClientEvent;
use roost_client_core::sync::decode::{
    DecodeRefusal, FIREHOSE_ARMS, SyncFrameMeta, decode_firehose,
};
use roost_client_core::{SyncDomain, SyncFrame};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::buffa::Message;
use roost_proto::{FirehoseFrame, KeepaliveFrame, TerminalTitleFrame};

use sync_decode_support::{
    SESSION, acked, application, closes, control, decoded, deliver, ready_core, refused, stamped,
};

fn title_arm(title: &str) -> Frame {
    Frame::TerminalTitle(Box::new(TerminalTitleFrame {
        session_id: SESSION.to_owned(),
        title: title.to_owned(),
        ..TerminalTitleFrame::default()
    }))
}

fn keepalive_arm() -> Frame {
    Frame::Keepalive(Box::new(KeepaliveFrame {
        ts: 1_700_000_000_000,
        ..KeepaliveFrame::default()
    }))
}

#[test]
fn the_host_generation_rides_the_decoded_event() {
    let event = decode_firehose(
        &application(SyncDomain::Terminal, 9, title_arm("vim")),
        SyncFrameMeta { generation: 42 },
    )
    .expect("a stamped application frame decodes");
    assert!(matches!(
        event,
        ClientEvent::SyncFrameReceived {
            generation: 42,
            delivery_seq: 9,
            frame: SyncFrame::TerminalTitle { .. },
        }
    ));
}

#[test]
fn bytes_that_are_not_a_frame_and_a_frame_with_no_arm_are_refused() {
    assert!(matches!(
        refused(&[0xff, 0xff, 0xff]),
        DecodeRefusal::Undecodable { .. }
    ));
    let empty = FirehoseFrame {
        delivery_seq: 4,
        domain: roost_proto::SyncDomain::Terminal.into(),
        domain_generation: 3,
        ..FirehoseFrame::default()
    }
    .encode_to_vec();
    assert_eq!(refused(&empty), DecodeRefusal::NoFrame);
}

#[test]
fn an_application_arm_without_a_sequence_or_a_domain_is_refused() {
    // v2: a `delivery_seq = 0` application frame is routed to handleV2Control,
    // whose default closes the link; an UNSPECIFIED domain is "malformed v2
    // application frame".
    for (domain, delivery_seq) in [
        (roost_proto::SyncDomain::Terminal, 0),
        (roost_proto::SyncDomain::Unspecified, 7),
    ] {
        assert!(
            matches!(
                refused(&stamped(domain, delivery_seq, 3, title_arm("x"))),
                DecodeRefusal::UnsequencedApplication {
                    arm: "terminal_title",
                    ..
                }
            ),
            "domain={domain:?} delivery_seq={delivery_seq}"
        );
    }
}

#[test]
fn a_control_arm_carrying_any_stamp_is_refused() {
    // v2 `handleV2Control`: "sequenced v2 control" for any of the three.
    for (domain, delivery_seq, domain_generation) in [
        (roost_proto::SyncDomain::Unspecified, 5, 0),
        (roost_proto::SyncDomain::Terminal, 0, 0),
        (roost_proto::SyncDomain::Unspecified, 0, 2),
    ] {
        let refusal = refused(&stamped(
            domain,
            delivery_seq,
            domain_generation,
            keepalive_arm(),
        ));
        assert!(
            matches!(
                refusal,
                DecodeRefusal::SequencedControl {
                    arm: "keepalive",
                    ..
                }
            ),
            "{refusal}"
        );
    }
    assert!(matches!(
        decoded(&control(keepalive_arm()), 1),
        ClientEvent::SyncFrameReceived {
            delivery_seq: 0,
            frame: SyncFrame::Keepalive,
            ..
        }
    ));
}

#[test]
fn a_refused_frame_closes_its_link_and_is_neither_applied_nor_acknowledged() {
    let (mut core, generation) = ready_core();
    let revision = core.store().revision();
    let refusal = refused(&stamped(
        roost_proto::SyncDomain::Terminal,
        0,
        3,
        title_arm("never"),
    ));
    let effects = core.handle(ClientEvent::SyncFrameRefused {
        generation,
        reason: refusal.to_string(),
    });
    assert!(closes(&effects, generation), "{effects:?}");
    assert!(acked(&effects).is_empty());
    assert!(core.store().terminal_titles.is_empty());
    assert!(core.store().revision() > revision, "the link state changed");
    assert!(
        !core.store().sync.accepts(generation),
        "the refused link stops accepting, as v2 sets accepting=false"
    );
    // Anything after the refusal on the same socket is neither applied nor
    // acknowledged.
    let after = deliver(
        &mut core,
        generation,
        &application(SyncDomain::Terminal, 10, title_arm("late")),
    );
    assert!(acked(&after).is_empty());
    assert!(core.store().terminal_titles.is_empty());
}

#[test]
fn a_refusal_for_a_replaced_generation_leaves_the_live_link_alone() {
    let (mut core, generation) = ready_core();
    let effects = core.handle(ClientEvent::SyncFrameRefused {
        generation: generation + 99,
        reason: "stale".to_owned(),
    });
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::CloseSyncLink { .. })),
        "{effects:?}"
    );
    assert!(core.store().sync.accepts(generation));
}

#[test]
fn an_applied_application_frame_is_acknowledged_by_its_sequence() {
    let (mut core, generation) = ready_core();
    let effects = deliver(
        &mut core,
        generation,
        &application(SyncDomain::Terminal, 11, title_arm("htop")),
    );
    assert_eq!(acked(&effects), vec![11]);
    assert_eq!(
        core.store()
            .terminal_titles
            .get(SESSION)
            .map(String::as_str),
        Some("htop")
    );
}

/// Every `FirehoseFrame` oneof arm `sync.proto` declares, by name and field.
fn declared_firehose_arms() -> BTreeMap<String, u32> {
    let proto = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol/proto/roost/v1/sync.proto"),
    )
    .expect("sync.proto is the wire contract and is checked in");
    let message = proto
        .split("message FirehoseFrame {")
        .nth(1)
        .expect("sync.proto declares FirehoseFrame");
    let oneof = message
        .split("oneof frame {")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("FirehoseFrame declares its oneof");
    oneof
        .lines()
        .map(|line| line.split("//").next().unwrap_or_default().trim())
        .filter(|line| line.ends_with(';'))
        .map(|line| {
            let words: Vec<&str> = line.trim_end_matches(';').split_whitespace().collect();
            match words.as_slice() {
                [_, name, "=", field] => {
                    ((*name).to_owned(), field.parse().expect("a field number"))
                }
                other => panic!("unexpected oneof line {other:?}"),
            }
        })
        .collect()
}

#[test]
fn every_proto_arm_is_in_the_table_and_the_table_names_no_other() {
    let declared = declared_firehose_arms();
    let mapped: BTreeMap<String, u32> = FIREHOSE_ARMS
        .iter()
        .map(|arm| (arm.name.to_owned(), arm.field))
        .collect();
    assert_eq!(mapped.len(), FIREHOSE_ARMS.len(), "no arm is listed twice");
    assert_eq!(
        declared, mapped,
        "sync.proto's FirehoseFrame oneof and sync::decode::arms disagree; a new arm \
         needs a SyncFrame variant, a row, and a fold"
    );
}
