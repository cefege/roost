//! OMP actor protocol, replay suppression, ACK retention and cancellation.
//! The real owner connects to a real SDK fake agent through duplex streams.
//! Frames are observed at the same uplink boundary as the coordinator link.

#[path = "omp_owner/duplex.rs"]
mod duplex;
#[path = "omp_owner/interactions.rs"]
mod interactions;
#[path = "omp_owner/ordering.rs"]
mod ordering;
#[path = "omp_owner/ui.rs"]
mod ui;
use duplex::{DuplexLauncher, MissingLauncher};
use roost_proto::{DOmpCancel, DOmpEventsAck, DOmpOpen, DOmpPrompt, DOmpSetMode, WOmpEvents};
use roost_protocol::wire::{agent_chat::OmpWorkerEvent, coord_worker::CoordWorkerUpstream};
use roost_worker::omp::launcher::{OmpLaunch, OmpLaunchError, OmpLauncher};
use roost_worker::{
    link_ports::OmpPort,
    omp::{OmpOwner, OmpResend},
    uplink::{self, UplinkReceiver},
};
use std::{sync::Arc, time::Duration};
use tokio::io::AsyncReadExt;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Debug, Default)]
struct FlakyStartup {
    connected: std::sync::atomic::AtomicBool,
    healthy: DuplexLauncher,
}

impl OmpLauncher for FlakyStartup {
    type Transport = <DuplexLauncher as OmpLauncher>::Transport;

    fn launch(&self, cwd: &str) -> Result<OmpLaunch<Self::Transport>, OmpLaunchError> {
        if self
            .connected
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return self.healthy.launch(cwd);
        }
        let (client, mut peer) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let mut byte = [0_u8; 1];
            let _ = peer.read_exact(&mut byte).await;
        });
        let (read, write) = tokio::io::split(client);
        Ok(OmpLaunch {
            transport: agent_client_protocol::ByteStreams::new(write.compat_write(), read.compat()),
            description: "startup disconnect".into(),
            catalog: None,
        })
    }
}

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
        let batch = tokio::time::timeout(Duration::from_secs(4), frame(receiver))
            .await
            .unwrap_or_else(|_| panic!("OMP turn stalled; events: {events:?}"));
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
    let owner = OmpOwner::new(
        Arc::new(DuplexLauncher::default()),
        uplink,
        Arc::new(Default::default()),
    );
    owner.prompt(prompt(""));
    let (frames, events) = turn(&mut receiver).await;
    let mut expected_seq = 1;
    for frame in &frames {
        assert_eq!(frame.first_seq, expected_seq);
        expected_seq += frame.events_json.len() as u64;
    }
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
    let owner = OmpOwner::new(
        Arc::new(DuplexLauncher::default()),
        uplink,
        Arc::new(Default::default()),
    );
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
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink, Arc::new(Default::default()));
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
    let owner = OmpOwner::new(
        Arc::new(MissingLauncher),
        uplink,
        Arc::new(Default::default()),
    );
    owner.prompt(prompt(""));
    let (_, events) = turn(&mut receiver).await;
    assert!(
        matches!(events.last(),Some(OmpWorkerEvent::TurnEnded {stop_reason,error:Some(_),outstanding:0,..}) if stop_reason == "error")
    );
}

#[tokio::test]
async fn omp_owner_opens_settings_without_a_prompt_and_reuses_the_session() {
    let launcher = Arc::new(DuplexLauncher::default());
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink, Arc::new(Default::default()));
    let open = DOmpOpen {
        conversation_id: "c1".into(),
        cwd: "/work".into(),
        ..Default::default()
    };
    owner.open(open.clone());
    let _ = frame(&mut receiver).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), launcher.started.notified())
            .await
            .is_err()
    );
    owner.open(open);
    let refreshed = frame(&mut receiver).await;
    let refreshed_events: Vec<OmpWorkerEvent> = refreshed
        .events_json
        .iter()
        .map(|event| serde_json::from_str(event).unwrap())
        .collect();
    assert!(text(&refreshed_events).is_empty());
    assert_eq!(launcher.loads.load(std::sync::atomic::Ordering::Relaxed), 1);
    owner.prompt(prompt(""));
    let (_, events) = turn(&mut receiver).await;
    assert!(matches!(
        events.last(),
        Some(OmpWorkerEvent::TurnEnded {
            outstanding: 0,
            error: None,
            ..
        })
    ));
}

#[tokio::test]
async fn omp_owner_defers_open_refresh_until_prompt_settles() {
    let launcher = Arc::new(DuplexLauncher {
        hold: true,
        ..Default::default()
    });
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink, Arc::new(Default::default()));
    owner.prompt(prompt(""));
    tokio::time::timeout(Duration::from_secs(5), launcher.started.notified())
        .await
        .unwrap();
    owner.open(DOmpOpen {
        conversation_id: "c1".into(),
        cwd: "/work".into(),
        acp_session_id: "s1".into(),
        ..Default::default()
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(launcher.loads.load(std::sync::atomic::Ordering::Relaxed), 0);

    owner.cancel(DOmpCancel {
        conversation_id: "c1".into(),
        ..Default::default()
    });
    let (_, events) = turn(&mut receiver).await;

    assert_eq!(text(&events), vec!["hi"]);
    let refreshed = frame(&mut receiver).await;
    let refreshed_events: Vec<OmpWorkerEvent> = refreshed
        .events_json
        .iter()
        .map(|event| serde_json::from_str(event).unwrap())
        .collect();
    assert_eq!(launcher.loads.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(text(&refreshed_events).is_empty());
    assert!(
        refreshed_events
            .iter()
            .any(|event| matches!(event, OmpWorkerEvent::SessionReady { .. }))
    );
}

#[tokio::test]
async fn omp_owner_failed_preparation_does_not_interrupt_the_queued_first_prompt() {
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(
        Arc::new(FlakyStartup::default()),
        uplink,
        Arc::new(Default::default()),
    );
    owner.open(DOmpOpen {
        conversation_id: "c1".into(),
        cwd: "/work".into(),
        ..Default::default()
    });
    owner.prompt(prompt(""));
    let (_, events) = turn(&mut receiver).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, OmpWorkerEvent::SessionFailed { .. }))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, OmpWorkerEvent::ProcessExited { .. }))
    );
    assert_eq!(text(&events), vec!["hi"]);
    assert!(matches!(
        events.last(),
        Some(OmpWorkerEvent::TurnEnded {
            error: None,
            outstanding: 0,
            ..
        })
    ));
}

#[tokio::test]
async fn omp_plan_prompt_applies_the_offered_mode_before_prompt_dispatch() {
    let (uplink, mut receiver) = uplink::channel();
    let launcher = DuplexLauncher {
        require_mode_before_prompt: true,
        ..Default::default()
    };
    let owner = OmpOwner::new(Arc::new(launcher), uplink, Arc::new(Default::default()));
    let mut request = prompt("");
    request.mode = "plan".into();
    owner.prompt(request);

    let (_, events) = turn(&mut receiver).await;
    assert!(events.iter().any(|event| matches!(
        event,
        OmpWorkerEvent::Modes { modes } if modes.iter().any(|mode| mode.value == "plan")
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        OmpWorkerEvent::Update { update }
            if update["currentModeId"] == "plan"
                || update["modeId"] == "plan"
    )));
}

#[tokio::test]
async fn omp_idle_mode_change_opens_session_without_starting_turn() {
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(
        Arc::new(DuplexLauncher::default()),
        uplink,
        Arc::new(Default::default()),
    );
    owner.set_mode(DOmpSetMode {
        conversation_id: "c1".into(),
        cwd: "/work".into(),
        mode: "plan".into(),
        ..Default::default()
    });
    let mut events = Vec::new();
    loop {
        let batch = frame(&mut receiver).await;
        let batch_events = batch
            .events_json
            .iter()
            .map(|event| serde_json::from_str::<OmpWorkerEvent>(event).unwrap())
            .collect::<Vec<_>>();
        let changed = batch_events.iter().any(|event| {
            matches!(
                event,
                OmpWorkerEvent::Update { update }
                    if update["currentModeId"] == "plan" || update["modeId"] == "plan"
            )
        });
        events.extend(batch_events);
        if changed {
            break;
        }
    }
    assert!(
        events
            .iter()
            .any(|event| matches!(event, OmpWorkerEvent::SessionReady { .. }))
    );
    assert!(events.iter().any(|event| matches!(event, OmpWorkerEvent::Modes { modes } if modes.iter().any(|mode| mode.value == "plan"))));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, OmpWorkerEvent::TurnStarted { .. }))
    );
}
