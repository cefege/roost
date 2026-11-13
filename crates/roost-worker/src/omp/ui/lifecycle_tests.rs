//! Interactive ACP completions flush ordered frames without waiting for a tick.
//! Internal acknowledgements leave streaming batches on their usual cadence.
//! Receipts still wait for the coordinator's durable acknowledgement.

use super::*;
use crate::uplink;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use std::sync::Arc;

#[test]
fn interactive_completion_flushes_ordered_frames_before_its_receipt() {
    let (uplink, mut receiver) = uplink::channel();
    let mut emitter = Emitter::new("conversation".into(), "run".into(), uplink);
    let mut ui = UiState::new(Arc::new(Default::default()), "owner".into());
    ui.transport_id = "transport".into();
    assert!(ui.receipts.track(
        "request",
        ViewOrigin {
            client_id: "client".into(),
            view_id: "view".into(),
            transport_id: "transport".into(),
            request_id: "request".into(),
        },
        true,
    ));
    emitter.emit(OmpWorkerEvent::Ui {
        message: json!({"verb":"first"}),
    });
    emitter.emit(OmpWorkerEvent::Ui {
        message: json!({"verb":"second"}),
    });

    ui.complete("event", "transport", "", Ok(json!({})), &mut emitter);
    assert!(receiver.try_recv_delivery().is_none());
    ui.complete("input", "transport", "request", Ok(json!({})), &mut emitter);
    let Some(delivery) = receiver.try_recv_delivery() else {
        panic!("interactive completion must deliver its pending frames");
    };
    let Some(CoordWorkerUpstream::OmpEvents(frame)) = delivery._take_unconfirmed_frame() else {
        panic!("expected an OMP event batch");
    };
    assert_eq!(frame.first_seq, 1);
    assert_eq!(frame.events_json.len(), 2);
    assert!(frame.events_json[0].contains("first"));
    assert!(frame.events_json[1].contains("second"));
    assert!(receiver.try_recv_delivery().is_none());
    ui.receipts.acknowledge(2, &emitter);
    let Some(receipt) = receiver.try_recv_delivery() else {
        panic!("durable acknowledgement must release the receipt");
    };
    assert!(matches!(
        receipt._take_unconfirmed_frame(),
        Some(CoordWorkerUpstream::OmpUiActionReceipt(_))
    ));
}
