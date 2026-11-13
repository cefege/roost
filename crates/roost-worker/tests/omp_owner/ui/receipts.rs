//! Private action receipts gated on the browser's durable frame acknowledgement.
//! A typed action the browser did not receipt still reports a native failure;
//! its success stays silent.

use super::*;

#[tokio::test]
async fn receipts_wait_for_durable_frame_acknowledgement() {
    let (launcher, mut requests) = UiLauncher::new();
    let launcher = Arc::new(launcher);
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink, Arc::new(Default::default()));
    let mut document = OmpUiDocument::default();
    owner.ui(open(
        "open-r",
        "device:a",
        "view-r",
        json!({
            "cols": 80, "rows": 24, "dark": true, "reduceMotion": false
        }),
    ));
    peer_request(&mut requests).await;
    let delivered = until(&mut receiver, &mut document, ui_frame).await;
    let last_seq = delivered.first_seq + delivered.events_json.len() as u64 - 1;

    owner.ui(DOmpUi {
        receipt: true,
        ..view_action(
            "input-r",
            "device:a",
            "view-r",
            "open-r",
            "input",
            json!({"data":"x"}),
        )
    });
    assert_eq!(peer_request(&mut requests).await.method, "_omp/ui/input");
    // Nothing may leave the worker until the browser confirms the frames.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), receiver.recv_delivery())
            .await
            .is_err(),
        "the receipt escaped before its frames were durable"
    );
    owner.ack(DOmpEventsAck {
        conversation_id: "c1".into(),
        run_id: delivered.run_id,
        seq: last_seq,
        ..Default::default()
    });
    let CoordWorkerUpstream::OmpUiActionReceipt(receipt) = next_upstream(&mut receiver).await
    else {
        panic!("action receipt frame");
    };
    assert_eq!(receipt.origin_request_id, "input-r");
    assert_eq!(receipt.view_id, "view-r");
    assert_eq!(receipt.transport_id, "open-r");
    assert_eq!(receipt.error, "");
    close(&owner);
}

#[tokio::test]
async fn a_failed_native_action_still_receipts_its_error_after_the_ack() {
    let (launcher, mut requests) = UiLauncher::new();
    let launcher = Arc::new(launcher);
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink, Arc::new(Default::default()));
    let mut document = OmpUiDocument::default();
    owner.ui(open(
        "open-f",
        "device:a",
        "view-f",
        json!({
            "cols": 80, "rows": 24, "dark": true, "reduceMotion": false
        }),
    ));
    peer_request(&mut requests).await;
    let delivered = until(&mut receiver, &mut document, ui_frame).await;
    owner.ui(DOmpUi {
        receipt: true,
        ..view_action(
            "input-f",
            "device:a",
            "view-f",
            "open-f",
            "input",
            json!({"data":"fail"}),
        )
    });
    assert_eq!(peer_request(&mut requests).await.method, "_omp/ui/input");
    owner.ack(DOmpEventsAck {
        conversation_id: "c1".into(),
        run_id: delivered.run_id.clone(),
        seq: delivered.first_seq + delivered.events_json.len() as u64 - 1,
        ..Default::default()
    });
    loop {
        match next_upstream(&mut receiver).await {
            CoordWorkerUpstream::OmpEvents(batch) => {
                // The failure notice arrives first and must also be acknowledged.
                let last = batch.first_seq + batch.events_json.len() as u64 - 1;
                owner.ack(DOmpEventsAck {
                    conversation_id: "c1".into(),
                    run_id: batch.run_id.clone(),
                    seq: last,
                    ..Default::default()
                });
            }
            CoordWorkerUpstream::OmpUiActionReceipt(receipt) => {
                assert_eq!(receipt.origin_request_id, "input-f");
                assert!(!receipt.error.is_empty());
                break;
            }
            other => panic!("unexpected frame {other:?}"),
        }
    }
    close(&owner);
}

#[tokio::test]
async fn a_typed_input_without_a_receipt_still_reports_its_native_failure() {
    let (launcher, mut requests) = UiLauncher::new();
    let launcher = Arc::new(launcher);
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink, Arc::new(Default::default()));
    let mut document = OmpUiDocument::default();
    owner.ui(open(
        "open-u",
        "device:a",
        "view-u",
        json!({
            "cols": 80, "rows": 24, "dark": true, "reduceMotion": false
        }),
    ));
    peer_request(&mut requests).await;
    let delivered = until(&mut receiver, &mut document, ui_frame).await;
    // The browser did not ask for a receipt, and the native handler rejects it
    // after dispatch: the private failure must still reach its own view.
    owner.ui(view_action(
        "input-u",
        "device:a",
        "view-u",
        "open-u",
        "input",
        json!({"data":"fail"}),
    ));
    assert_eq!(peer_request(&mut requests).await.method, "_omp/ui/input");
    owner.ack(DOmpEventsAck {
        conversation_id: "c1".into(),
        run_id: delivered.run_id,
        seq: delivered.first_seq + delivered.events_json.len() as u64 - 1,
        ..Default::default()
    });
    let mut reported = false;
    loop {
        match next_upstream(&mut receiver).await {
            CoordWorkerUpstream::OmpEvents(batch) => {
                reported |= batch.events_json.iter().any(|value| {
                    matches!(
                        serde_json::from_str::<OmpWorkerEvent>(value),
                        Ok(OmpWorkerEvent::UiFailed { .. })
                    )
                });
                let last = batch.first_seq + batch.events_json.len() as u64 - 1;
                owner.ack(DOmpEventsAck {
                    conversation_id: "c1".into(),
                    run_id: batch.run_id.clone(),
                    seq: last,
                    ..Default::default()
                });
            }
            CoordWorkerUpstream::OmpUiActionReceipt(receipt) => {
                assert!(
                    reported,
                    "the failure receipt escaped before the failure notice"
                );
                assert_eq!(receipt.origin_request_id, "input-u");
                assert_eq!(receipt.client_id, "device:a");
                assert_eq!(receipt.view_id, "view-u");
                assert_eq!(receipt.transport_id, "open-u");
                assert!(!receipt.error.is_empty());
                break;
            }
            other => panic!("unexpected frame {other:?}"),
        }
    }
    close(&owner);
}

#[tokio::test]
async fn a_typed_input_success_without_a_receipt_stays_silent() {
    let (launcher, mut requests) = UiLauncher::new();
    let launcher = Arc::new(launcher);
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink, Arc::new(Default::default()));
    let mut document = OmpUiDocument::default();
    owner.ui(open(
        "open-s",
        "device:a",
        "view-s",
        json!({
            "cols": 80, "rows": 24, "dark": true, "reduceMotion": false
        }),
    ));
    peer_request(&mut requests).await;
    let delivered = until(&mut receiver, &mut document, ui_frame).await;
    owner.ui(view_action(
        "input-silent",
        "device:a",
        "view-s",
        "open-s",
        "input",
        json!({"data":"ok"}),
    ));
    assert_eq!(peer_request(&mut requests).await.method, "_omp/ui/input");
    // A receipt-tracked success completes after the silent one; its receipt is
    // the fence that proves both native handlers finished. The worker releases
    // every due receipt in one acknowledgement sweep, so a silent success
    // receipt would already be queued when the fence arrives.
    owner.ui(DOmpUi {
        receipt: true,
        ..view_action(
            "input-b",
            "device:a",
            "view-s",
            "open-s",
            "input",
            json!({"data":"b"}),
        )
    });
    assert_eq!(peer_request(&mut requests).await.method, "_omp/ui/input");
    owner.ack(DOmpEventsAck {
        conversation_id: "c1".into(),
        run_id: delivered.run_id,
        seq: delivered.first_seq + delivered.events_json.len() as u64 - 1,
        ..Default::default()
    });
    loop {
        match next_upstream(&mut receiver).await {
            CoordWorkerUpstream::OmpEvents(batch) => {
                let last = batch.first_seq + batch.events_json.len() as u64 - 1;
                owner.ack(DOmpEventsAck {
                    conversation_id: "c1".into(),
                    run_id: batch.run_id.clone(),
                    seq: last,
                    ..Default::default()
                });
            }
            CoordWorkerUpstream::OmpUiActionReceipt(receipt) => {
                assert_eq!(receipt.origin_request_id, "input-b");
                assert_eq!(receipt.error, "");
                break;
            }
            other => panic!("unexpected frame {other:?}"),
        }
    }
    while let Some(delivery) = receiver.try_recv_delivery() {
        if let Some(CoordWorkerUpstream::OmpUiActionReceipt(receipt)) =
            delivery._take_unconfirmed_frame()
        {
            panic!(
                "a successful input without a receipt reported {:?}",
                receipt.origin_request_id
            );
        }
    }
    close(&owner);
}
