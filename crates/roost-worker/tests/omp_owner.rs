//! OMP actor protocol, replay suppression, ACK retention and cancellation.
//! The real owner connects to a real SDK fake agent through duplex streams.
//! Frames are observed at the same uplink boundary as the coordinator link.

#[path = "omp_owner/duplex.rs"]
mod duplex;
#[path = "omp_owner/ordering.rs"]
mod ordering;
use duplex::{DuplexLauncher, MissingLauncher};
use roost_proto::{DOmpCancel, DOmpEventsAck, DOmpPrompt, WOmpEvents};
use roost_protocol::wire::{agent_chat::OmpWorkerEvent, coord_worker::CoordWorkerUpstream};
use roost_worker::{
    link_ports::OmpPort,
    omp::{OmpOwner, OmpResend},
    uplink::{self, UplinkReceiver},
};
use std::{sync::Arc, time::Duration};

fn prompt(session: &str) -> DOmpPrompt {
    DOmpPrompt {
        conversation_id: "c1".into(),
        request_id: "req1".into(),
        cwd: "/work".into(),
        acp_session_id: session.into(),
        text: "hello".into(),
        ..Default::default()
    }
}
async fn frame(receiver: &mut UplinkReceiver) -> WOmpEvents {
    let delivery = tokio::time::timeout(Duration::from_secs(5), receiver.recv_delivery())
        .await
        .expect("frame deadline")
        .expect("uplink");
    match delivery
        ._take_unconfirmed_frame()
        .expect("unconfirmed frame")
    {
        CoordWorkerUpstream::OmpEvents(frame) => frame,
        _ => panic!("OMP frame"),
    }
}
async fn turn(receiver: &mut UplinkReceiver) -> (Vec<WOmpEvents>, Vec<OmpWorkerEvent>) {
    let mut frames = vec![];
    let mut events = vec![];
    loop {
        let batch = frame(receiver).await;
        let batch_events: Vec<OmpWorkerEvent> = batch
            .events_json
            .iter()
            .map(|json| serde_json::from_str(json).unwrap())
            .collect();
        let ended = batch_events
            .iter()
            .any(|event| matches!(event, OmpWorkerEvent::TurnEnded { .. }));
        events.extend(batch_events);
        frames.push(batch);
        if ended {
            return (frames, events);
        }
    }
}
fn text(events: &[OmpWorkerEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            OmpWorkerEvent::Update { update } => update["content"]["text"].as_str(),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn omp_owner_streams_contiguous_events_resends_and_deduplicates() {
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::new(DuplexLauncher::default()), uplink);
    owner.prompt(prompt(""));
    let (frames, events) = turn(&mut receiver).await;
    let mut expected_seq = 1;
    for frame in &frames {
        assert_eq!(frame.first_seq, expected_seq);
        expected_seq += frame.events_json.len() as u64;
    }
    assert!(
        matches!(&events[0],OmpWorkerEvent::SessionReady {acp_session_id,..} if acp_session_id == "s1")
    );
    assert!(matches!(&events[1],OmpWorkerEvent::TurnStarted {request_id} if request_id == "req1"));
    assert_eq!(text(&events), vec!["hi"]);
    assert!(
        matches!(events.last(),Some(OmpWorkerEvent::TurnEnded {outstanding:0,stop_reason,..}) if stop_reason == "end_turn")
    );
    owner.resend_all();
    for original in &frames {
        assert_eq!(frame(&mut receiver).await, *original);
    }
    owner.ack(DOmpEventsAck {
        conversation_id: "c1".into(),
        run_id: frames[0].run_id.clone(),
        seq: expected_seq - 1,
        ..Default::default()
    });
    owner.prompt(prompt(""));
    owner.resend_all();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), receiver.recv_delivery())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn omp_owner_load_suppresses_history_and_keeps_new_output() {
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::new(DuplexLauncher::default()), uplink);
    owner.prompt(prompt("s1"));
    let (_, events) = turn(&mut receiver).await;
    assert_eq!(text(&events), vec!["hi"]);
}

#[tokio::test]
async fn omp_owner_cancel_reaches_the_live_session() {
    let launcher = Arc::new(DuplexLauncher {
        hold: true,
        ..Default::default()
    });
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink);
    owner.prompt(prompt(""));
    tokio::time::timeout(Duration::from_secs(5), launcher.started.notified())
        .await
        .unwrap();
    owner.cancel(DOmpCancel {
        conversation_id: "c1".into(),
        ..Default::default()
    });
    tokio::time::timeout(Duration::from_secs(5), launcher.cancelled.notified())
        .await
        .unwrap();
    let (_, events) = turn(&mut receiver).await;
    assert!(
        matches!(events.last(),Some(OmpWorkerEvent::TurnEnded {stop_reason,outstanding:0,..}) if stop_reason == "cancelled")
    );
}

#[tokio::test]
async fn omp_owner_missing_install_settles_failed() {
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::new(MissingLauncher), uplink);
    owner.prompt(prompt(""));
    let (_, events) = turn(&mut receiver).await;
    assert!(
        matches!(&events[0],OmpWorkerEvent::Notice {title,..} if title == "OMP is not installed")
    );
    assert!(
        matches!(events.last(),Some(OmpWorkerEvent::TurnEnded {error:Some(error),outstanding:0,..}) if error == "omp not found")
    );
}
