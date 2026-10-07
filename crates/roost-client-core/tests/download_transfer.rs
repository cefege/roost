//! The download run's lifecycle: chunks assemble the exact file and drive the
//! card to `Done` on the coordinator route, an inconsistent answer fails the
//! card with its reason, and a dismissed card cancels the run.

use roost_client_core::ClientCore;
use roost_client_core::client::download::{
    DOWNLOAD_ROUTE, DownloadError, DownloadProgress, DownloadRun, MAX_DOWNLOAD_BYTES,
};
use roost_client_core::client::rpc::calls::files::FileChunk;
use roost_client_core::store::transfers::{
    TransferDirection, TransferRoute, TransferState, remove_transfer,
};

fn chunk(data: &[u8], size: u64, eof: bool) -> FileChunk {
    FileChunk {
        data: data.to_vec(),
        size,
        eof,
    }
}

#[test]
fn chunks_assemble_the_file_and_settle_done_via_coordinator() {
    let mut core = ClientCore::in_memory("tab-download");
    let mut run = DownloadRun::begin(core.store_mut(), "dl-1", "notes.txt", 100);
    let card = core.store().transfers.transfer("dl-1").expect("card");
    assert_eq!(card.direction, TransferDirection::Down);
    assert_eq!(card.state, TransferState::Queued);
    assert_eq!(card.route, Some(TransferRoute::Coordinator));
    assert_eq!(DOWNLOAD_ROUTE, TransferRoute::Coordinator);
    assert_eq!(run.next_offset(), 0);

    let first = run.accept_chunk(core.store_mut(), chunk(b"hello ", 11, false), 200);
    assert_eq!(first, Ok(DownloadProgress::More { offset: 6 }));
    let card = core.store().transfers.transfer("dl-1").expect("card");
    assert_eq!(card.state, TransferState::Running);
    assert_eq!((card.bytes_done, card.bytes_total), (6, 11));

    let last = run.accept_chunk(core.store_mut(), chunk(b"world", 11, true), 300);
    assert_eq!(
        last,
        Ok(DownloadProgress::Complete(b"hello world".to_vec()))
    );
    assert!(
        core.store()
            .transfers
            .transfer("dl-1")
            .expect("card")
            .speed_bps
            .is_some()
    );

    run.finish(core.store_mut(), true, 400);
    let card = core.store().transfers.transfer("dl-1").expect("card");
    assert_eq!(card.state, TransferState::Done);
    assert_eq!(card.bytes_done, 11);
}

#[test]
fn an_empty_file_completes_on_its_first_answer() {
    let mut core = ClientCore::in_memory("tab-download");
    let mut run = DownloadRun::begin(core.store_mut(), "dl-0", "empty", 100);
    let answer = run.accept_chunk(core.store_mut(), chunk(b"", 0, true), 200);
    assert_eq!(answer, Ok(DownloadProgress::Complete(Vec::new())));
}

#[test]
fn a_short_end_fails_the_card_with_its_reason() {
    let mut core = ClientCore::in_memory("tab-download");
    let mut run = DownloadRun::begin(core.store_mut(), "dl-2", "video.mkv", 100);
    let error = run
        .accept_chunk(core.store_mut(), chunk(b"abc", 10, true), 200)
        .expect_err("an eof short of the size is a truncation");
    assert_eq!(
        error,
        DownloadError::Truncated {
            received: 3,
            size: 10
        }
    );
    run.fail(core.store_mut(), &error.to_string(), 300);
    let card = core.store().transfers.transfer("dl-2").expect("card stays");
    assert_eq!(card.state, TransferState::Failed);
    assert_eq!(card.err.as_deref(), Some("the file ended at 3 of 10 bytes"));
}

#[test]
fn a_size_change_mid_run_and_an_oversized_file_are_refused() {
    let mut core = ClientCore::in_memory("tab-download");
    let mut run = DownloadRun::begin(core.store_mut(), "dl-3", "log", 100);
    assert!(
        run.accept_chunk(core.store_mut(), chunk(b"ab", 4, false), 200)
            .is_ok()
    );
    assert_eq!(
        run.accept_chunk(core.store_mut(), chunk(b"cd", 5, false), 300),
        Err(DownloadError::SizeChanged { first: 4, now: 5 })
    );
    let mut huge = DownloadRun::begin(core.store_mut(), "dl-4", "disk.img", 100);
    assert_eq!(
        huge.accept_chunk(
            core.store_mut(),
            chunk(b"", MAX_DOWNLOAD_BYTES + 1, false),
            200
        ),
        Err(DownloadError::TooLarge {
            size: MAX_DOWNLOAD_BYTES + 1
        })
    );
}

#[test]
fn a_refused_save_fails_the_card() {
    let mut core = ClientCore::in_memory("tab-download");
    let mut run = DownloadRun::begin(core.store_mut(), "dl-5", "a.bin", 100);
    assert!(
        run.accept_chunk(core.store_mut(), chunk(b"x", 1, true), 200)
            .is_ok()
    );
    run.finish(core.store_mut(), false, 300);
    let card = core.store().transfers.transfer("dl-5").expect("card");
    assert_eq!(card.state, TransferState::Failed);
}

#[test]
fn dismissing_the_card_cancels_the_run() {
    let mut core = ClientCore::in_memory("tab-download");
    let mut run = DownloadRun::begin(core.store_mut(), "dl-6", "big.tgz", 100);
    assert!(
        run.accept_chunk(core.store_mut(), chunk(b"ab", 4, false), 200)
            .is_ok()
    );
    assert!(remove_transfer(core.store_mut(), "dl-6"));
    assert!(run.is_cancelled(core.store()));
    assert_eq!(
        run.accept_chunk(core.store_mut(), chunk(b"cd", 4, true), 300),
        Err(DownloadError::Cancelled)
    );
    run.fail(core.store_mut(), "download cancelled", 400);
    assert!(
        core.store().transfers.transfer("dl-6").is_none(),
        "no card resurrected"
    );
}
