//! What may be typed into a PTY, what a grant authorises, and what a card says
//! when an upload settles.
//!
//! The defect class these prevent is an upload path that reaches a shell
//! unquoted or unescaped, and a grant used for an upload it does not name. The
//! chunking and its digest rule live in `attachments_transfer.rs`.
//!
//! The v2 names are the deliverable and are kept verbatim, so a reader holding
//! `apps/web/tests/client/attachments/attachmentInsertion.test.ts` can pair them
//! one for one.

use roost_client_core::ClientCore;
use roost_client_core::client::attachments::grant::{
    AttachmentDirectGrant, AttachmentDirectGrantRequest, AttachmentDirectGrantResponse,
};
use roost_client_core::client::attachments::insertion::safe_attachment_insertion;
use roost_client_core::client::attachments::transfer::ledger::{
    begin_upload_card, record_upload_progress, settle_upload_card,
};
use roost_client_core::client::attachments::transfer::{AttachmentTransferResult, is_chunk_sha256};
use roost_client_core::store::transfers::TransferState;

/// One lowercase hex SHA-256, which is the only digest shape the pipeline
/// accepts. Two of them, so a test that mixes them up fails.
const FIRST_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const SECOND_DIGEST: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

/// The path a worker commits to in these tests.
const WORKER_PATH: &str = "/worker/received.bin";

// ---------------------------------------------------------------- insertion

#[test]
fn quotes_a_path_with_a_space_as_one_inert_posix_argument() {
    for os in ["linux", "darwin"] {
        assert_eq!(
            safe_attachment_insertion(os, "/tmp/a b.txt").as_deref(),
            Some("'/tmp/a b.txt'"),
            "platform was {os}"
        );
    }
}

#[test]
fn quotes_a_path_with_an_embedded_quote_as_one_inert_posix_argument() {
    for os in ["linux", "darwin"] {
        assert_eq!(
            safe_attachment_insertion(os, "/tmp/it's.txt").as_deref(),
            Some("'/tmp/it'\"'\"'s.txt'"),
            "platform was {os}"
        );
    }
}

#[test]
fn quotes_a_path_with_a_command_substitution_as_one_inert_posix_argument() {
    for os in ["linux", "darwin"] {
        assert_eq!(
            safe_attachment_insertion(os, "/tmp/$(printf exploited)").as_deref(),
            Some("'/tmp/$(printf exploited)'"),
            "platform was {os}"
        );
    }
}

#[test]
fn quotes_a_path_with_a_backtick_substitution_as_one_inert_posix_argument() {
    for os in ["linux", "darwin"] {
        assert_eq!(
            safe_attachment_insertion(os, "/tmp/`printf exploited`").as_deref(),
            Some("'/tmp/`printf exploited`'"),
            "platform was {os}"
        );
    }
}

#[test]
fn quotes_a_path_with_non_ascii_characters_as_one_inert_posix_argument() {
    for os in ["linux", "darwin"] {
        assert_eq!(
            safe_attachment_insertion(os, "/tmp/你好 🐓.txt").as_deref(),
            Some("'/tmp/你好 🐓.txt'"),
            "platform was {os}"
        );
    }
}

#[test]
fn rejects_every_c0_del_and_c1_control_character() {
    // Every code point from NUL through US, then DEL through the end of C1 —
    // including the two that are invisible in a source file.
    for code_point in (0u32..0x20).chain(0x7f..0xa0) {
        let character = char::from_u32(code_point).expect("a control code point");
        let abs_path = format!("/tmp/before{character}after");
        assert_eq!(
            safe_attachment_insertion("linux", &abs_path),
            None,
            "U+{code_point:04X} must not reach a PTY"
        );
        assert_eq!(safe_attachment_insertion("darwin", &abs_path), None);
    }
}

#[test]
fn returns_null_on_windows_without_rewriting_the_path() {
    assert_eq!(
        safe_attachment_insertion("win32", r"C:\Users\alice\report.txt"),
        None
    );
    assert_eq!(
        safe_attachment_insertion("win32", "/posix-looking/path.txt"),
        None
    );
}

// -------------------------------------------------------------------- grant

/// The exact tuple these tests' grants name.
fn grant_request() -> AttachmentDirectGrantRequest {
    AttachmentDirectGrantRequest {
        worker_fp: "worker-a".to_owned(),
        session_id: "session-a".to_owned(),
        upload_id: "upload-a".to_owned(),
        filename: "direct.bin".to_owned(),
        short_path: false,
        total_bytes: 2,
    }
}

fn grant_response() -> AttachmentDirectGrantResponse {
    AttachmentDirectGrantResponse {
        grant_id: "grant-a".to_owned(),
        secret: "secret-a".to_owned(),
        worker_epoch: "epoch-a".to_owned(),
        peer_supported: true,
        stun_urls: Vec::new(),
    }
}

#[test]
fn a_grant_admits_only_the_upload_it_names() {
    let request = grant_request();
    let grant = AttachmentDirectGrant::from_response(
        request.clone(),
        "tab-a",
        "device-a",
        grant_response(),
    )
    .expect("a complete answer is a grant");
    assert!(grant.admits(&request));

    let mut other = request.clone();
    other.upload_id = "upload-b".to_owned();
    assert!(
        !grant.admits(&other),
        "a grant for a different upload authorises nothing here"
    );

    let mut wider = request.clone();
    wider.total_bytes = 3;
    assert!(
        !grant.admits(&wider),
        "the grant's total is what the worker will hold this file to"
    );
}

#[test]
fn an_answer_without_a_secret_is_not_a_grant() {
    // A grant is folded from an answer that EXISTS. The no-answer case is not
    // this function's to decide: `mint_grant` reports it as `None` and
    // `attachments_fallback.rs` pins what the caller then does with it.
    let mut response = grant_response();
    response.secret = String::new();
    assert_eq!(
        AttachmentDirectGrant::from_response(grant_request(), "tab-a", "device-a", response),
        None
    );
}

#[test]
fn a_grants_hello_names_the_whole_authenticated_tuple() {
    let grant = AttachmentDirectGrant::from_response(
        grant_request(),
        "tab-a",
        "device-a",
        grant_response(),
    )
    .expect("a complete answer is a grant");
    let hello = grant.hello("peer-a");
    assert_eq!(hello.grant_id, "grant-a");
    assert_eq!(hello.secret, "secret-a");
    assert_eq!(hello.tab_id, "tab-a");
    assert_eq!(hello.device_fingerprint, "device-a");
    assert_eq!(hello.session_id, "session-a");
    assert_eq!(hello.upload_id, "upload-a");
    assert_eq!(hello.filename, "direct.bin");
    assert!(!hello.short_path);
    assert_eq!(hello.total_bytes, 2);
    assert_eq!(hello.peer_id, "peer-a");
    assert_eq!(hello.worker_epoch, "epoch-a");
    assert!(
        grant.hello("").peer_id.is_empty(),
        "loopback authenticates with an empty peer id"
    );
}

// ------------------------------------------------------------------- ledger

#[test]
fn an_upload_card_reports_acknowledged_bytes_and_settles_once() {
    let mut core = ClientCore::in_memory("tab-attachments");
    begin_upload_card(core.store_mut(), "upload-a", "received.bin", 1_000, 0);
    assert!(record_upload_progress(
        core.store_mut(),
        "upload-a",
        512,
        10
    ));
    assert!(record_upload_progress(
        core.store_mut(),
        "upload-a",
        1_000,
        20
    ));
    let card = core
        .store()
        .transfers
        .transfer("upload-a")
        .expect("the card is there");
    assert_eq!(card.bytes_done, 1_000);
    assert_eq!(card.state, TransferState::Running);

    let outcome = AttachmentTransferResult {
        abs_path: WORKER_PATH.to_owned(),
    };
    assert!(settle_upload_card(
        core.store_mut(),
        "upload-a",
        Ok(&outcome),
        30
    ));
    assert_eq!(
        core.store()
            .transfers
            .transfer("upload-a")
            .map(|card| card.state),
        Some(TransferState::Done)
    );
}

#[test]
fn a_digest_is_accepted_only_as_lowercase_hex_sha256() {
    assert!(is_chunk_sha256(FIRST_DIGEST));
    assert!(is_chunk_sha256(SECOND_DIGEST));
    assert!(
        !is_chunk_sha256(&FIRST_DIGEST.to_uppercase()),
        "two spellings of one digest would look like corruption"
    );
    assert!(!is_chunk_sha256(&FIRST_DIGEST[..63]));
    assert!(!is_chunk_sha256(&format!("{FIRST_DIGEST}0")));
    assert!(!is_chunk_sha256(&FIRST_DIGEST.replace('0', "z")));
}
