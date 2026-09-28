//! An OSC 7 report of a NEW folder publishes one `cwd` session event naming the
//! session, the folder and the chunk's time, through the manager's event sink;
//! the same folder again, or a chunk with no report, publishes nothing. Both
//! ingest lanes (live and capture) publish. Ports v2
//! `apps/worker/src/session/session-scrollback.ts:178-185` (`scanStreamState`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_support;

use std::sync::Arc;

use roost_protocol::wire::event::SessionEvent;
use roost_worker::session::cwd_events::CwdEventLane;
use roost_worker::session::emit::CellEmitter;

use session_support::{Harness, NOW, SESSION};

#[tokio::test]
async fn a_new_osc7_folder_publishes_one_cwd_event_and_a_repeat_publishes_none() {
    let harness = Harness::new();
    harness.install(SESSION, 7, "/home/user/project", "/home/user/project");
    let (lane, writer) = CwdEventLane::new();
    let mut emitter = CellEmitter::new();
    emitter.attach_cwd_events(lane);
    let record = harness.table.record_of_channel(7).unwrap();
    {
        let mut record = record.lock().unwrap();
        emitter.ingest_pty_chunk(
            &mut record,
            b"$ cd /tmp/next\r\n\x1b]7;file://host/tmp/next\x07",
            NOW,
        );
        emitter.ingest_pty_chunk(&mut record, b"\x1b]7;file://host/tmp/next\x07", NOW + 1);
        emitter.retain_without_parsing(&mut record, b"\x1b]7;file://host/tmp/other\x07", NOW + 2);
        emitter.ingest_pty_chunk(&mut record, b"no report here", NOW + 3);
        assert_eq!(
            record.identity.cwd, "/tmp/other",
            "the snapshot reads the newest folder"
        );
    }
    // Every lane is gone once the emitter is, so the writer drains and ends.
    drop(emitter);
    writer.run(Arc::clone(&harness.manager)).await;
    let published: Vec<(String, String, i64)> = harness
        .sink
        .published()
        .into_iter()
        .filter_map(|event| match event {
            SessionEvent::Cwd {
                session_id,
                cwd,
                ts,
                ..
            } => Some((session_id.to_string(), cwd, ts)),
            _ => None,
        })
        .collect();
    assert_eq!(
        published,
        [
            (SESSION.to_owned(), "/tmp/next".to_owned(), NOW),
            (SESSION.to_owned(), "/tmp/other".to_owned(), NOW + 2),
        ]
    );
}
