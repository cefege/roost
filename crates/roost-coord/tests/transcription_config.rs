//! The settings surface: what a stored config reads as, what a write changes,
//! and what the key handoff hands over to an authenticated browser.
//!
//! Each case is a boundary a browser can hit rather than a restatement of the
//! code: the mask is the only part of a credential a config response may carry,
//! an absent key and a blank key are different instructions, and a row under
//! another dashboard is not this deployment's to read.

// A test that cannot say what it expected is not a test. `expect` is denied
// outside `#[cfg(test)]`, and an integration test is its own crate, so the
// exemption has to be stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod transcription_fixture;

use connectrpc::ErrorCode;
use roost_coord::diagnostics::rpc_transcription::{
    handle_transcription_get_config, handle_transcription_grant_token,
    handle_transcription_set_config, handle_transcription_test,
};
use roost_proto as proto;
use transcription_fixture::{KEY_TAIL, STORED_KEY, TranscriptionFixture};

/// A config that was never written reads as unconfigured, in English.
#[tokio::test]
async fn a_config_that_was_never_written_reads_as_unconfigured_in_english() {
    let fixture = TranscriptionFixture::new("unset").await;

    let config = handle_transcription_get_config(
        &fixture.core,
        &fixture.browser(),
        proto::TranscriptionGetConfigRequest::default(),
    )
    .await
    .expect("an unset config is readable")
    .body;

    assert!(!config.deepgram_configured, "no key was ever stored");
    assert_eq!(config.deepgram_key_masked, "", "an unset key has no mask");
    assert_eq!(config.deepgram_language, "en", "the default language");
}

/// A written key comes back masked, and no response ever carries the whole key.
#[tokio::test]
async fn a_written_key_comes_back_masked_and_never_whole() {
    let fixture = TranscriptionFixture::new("mask").await;

    let config = handle_transcription_set_config(
        &fixture.core,
        &fixture.browser(),
        set_request(Some(&format!("  {STORED_KEY}  ")), "multi"),
    )
    .await
    .expect("a config can be written")
    .body;

    assert!(config.deepgram_configured);
    assert_eq!(
        config.deepgram_key_masked,
        format!("····{KEY_TAIL}"),
        "four characters of the key, and the prefix that says it is masked"
    );
    assert_eq!(config.deepgram_language, "multi");
    assert!(
        !config.deepgram_key_masked.contains("secret"),
        "a masked key must not leak its body: {}",
        config.deepgram_key_masked
    );

    let read_back = handle_transcription_get_config(
        &fixture.core,
        &fixture.browser(),
        proto::TranscriptionGetConfigRequest::default(),
    )
    .await
    .expect("a written config is readable")
    .body;
    assert_eq!(
        read_back.deepgram_key_masked, config.deepgram_key_masked,
        "the mask a write returns is the mask a read returns"
    );
}

/// A key shorter than the mask window is shown whole, and the window is what
/// the mask is built from.
///
/// `transcription.ts:51` masks with `key.slice(-4)`, which for a key shorter
/// than four characters returns the key. So the invariant the neighbouring test
/// asserts is narrower than it reads: a LONG key never travels whole, and no
/// key at all travels whole only once it is longer than the window. A Deepgram
/// key is always far longer than four characters, so this is parity rather than
/// a leak in practice -- but it is a bound derived from the element's own
/// length, and that is the shape that hides: `saturating_sub` is the only thing
/// between this line and a subtraction overflow on every short key.
#[tokio::test]
async fn a_key_shorter_than_the_mask_window_is_shown_whole() {
    let fixture = TranscriptionFixture::new("short").await;

    let config = handle_transcription_set_config(
        &fixture.core,
        &fixture.browser(),
        set_request(Some("ab"), "en"),
    )
    .await
    .expect("a short key is stored")
    .body;

    assert!(config.deepgram_configured, "a stored key is configured");
    assert_eq!(
        config.deepgram_key_masked, "····ab",
        "the window is bounded by the key's own length, as v2's slice(-4) is"
    );
}

/// A key exactly as long as the mask window is shown whole, and one character
/// longer is not.
#[tokio::test]
async fn the_mask_window_boundary_is_inclusive_of_the_whole_key() {
    let fixture = TranscriptionFixture::new("boundary").await;

    let exact = handle_transcription_set_config(
        &fixture.core,
        &fixture.browser(),
        set_request(Some("abcd"), "en"),
    )
    .await
    .expect("a four character key is stored")
    .body;
    assert_eq!(exact.deepgram_key_masked, "····abcd");

    let longer = handle_transcription_set_config(
        &fixture.core,
        &fixture.browser(),
        set_request(Some("abcde"), "en"),
    )
    .await
    .expect("a five character key is stored")
    .body;
    assert_eq!(
        longer.deepgram_key_masked, "····bcde",
        "one character past the window, the first is dropped"
    );
}

/// An absent key leaves the stored one alone; a blank one clears it.
///
/// These are the proto3 optional's two meanings and they are not
/// interchangeable: a settings pane that saves the language alone must not
/// silently wipe the credential, and one that emptied the field must not keep
/// handing it out.
#[tokio::test]
async fn an_absent_key_is_left_alone_and_a_blank_one_clears_it() {
    let fixture = TranscriptionFixture::new("optional").await;
    let caller = fixture.browser();
    write_key(&fixture, &caller).await;

    let after_language_only =
        handle_transcription_set_config(&fixture.core, &caller, set_request(None, "de"))
            .await
            .expect("a language-only save is accepted")
            .body;
    assert!(
        after_language_only.deepgram_configured,
        "a save that named no key must keep the stored one"
    );
    assert_eq!(after_language_only.deepgram_language, "de");

    let after_blank =
        handle_transcription_set_config(&fixture.core, &caller, set_request(Some("   "), "de"))
            .await
            .expect("a blank key is accepted as a clear")
            .body;
    assert!(
        !after_blank.deepgram_configured,
        "a blank key clears the credential"
    );
    assert_eq!(after_blank.deepgram_key_masked, "");
}

/// A language that is blank or only whitespace reads back as the default.
#[tokio::test]
async fn a_blank_language_is_stored_as_the_default() {
    let fixture = TranscriptionFixture::new("language").await;

    let config = handle_transcription_set_config(
        &fixture.core,
        &fixture.browser(),
        set_request(Some(STORED_KEY), "   "),
    )
    .await
    .expect("a blank language is accepted")
    .body;

    assert_eq!(
        config.deepgram_language, "en",
        "a blank language must not store a blank language"
    );
}

/// The handoff is the configured key itself, with no grant to expire.
#[tokio::test]
async fn the_handoff_is_the_configured_key_itself_and_never_a_temporary_grant() {
    let fixture = TranscriptionFixture::new("handoff").await;
    let caller = fixture.browser();
    write_key(&fixture, &caller).await;

    let grant = handle_transcription_grant_token(
        &fixture.core,
        &caller,
        proto::TranscriptionGrantTokenRequest::default(),
    )
    .await
    .expect("a configured key is handed over")
    .body;

    assert_eq!(
        grant.access_token, STORED_KEY,
        "the browser connects with it"
    );
    assert_eq!(
        grant.expires_in, 0,
        "a restricted key cannot mint a grant, so nothing expires here"
    );
}

/// A handoff before a key is pasted stops the browser asking; it is not a
/// transport fault, and a client that saw one would retry forever.
#[tokio::test]
async fn a_handoff_before_a_key_is_pasted_is_a_failed_precondition() {
    let fixture = TranscriptionFixture::new("no-key").await;

    let grant = handle_transcription_grant_token(
        &fixture.core,
        &fixture.browser(),
        proto::TranscriptionGrantTokenRequest::default(),
    )
    .await
    .expect_err("there is no key to hand over");

    assert_eq!(grant.code, ErrorCode::FailedPrecondition);
    assert_eq!(grant.message.as_deref(), Some("Deepgram not configured"));
}

/// A key stored under another dashboard is not this deployment's credential.
#[tokio::test]
async fn a_key_stored_under_another_dashboard_does_not_answer() {
    let fixture = TranscriptionFixture::new("foreign").await;
    fixture.seed_foreign_key().await;

    let config = handle_transcription_get_config(
        &fixture.core,
        &fixture.browser(),
        proto::TranscriptionGetConfigRequest::default(),
    )
    .await
    .expect("an unset config is still readable")
    .body;

    assert!(
        !config.deepgram_configured,
        "another dashboard's key must not read as configured here"
    );

    let grant = handle_transcription_grant_token(
        &fixture.core,
        &fixture.browser(),
        proto::TranscriptionGrantTokenRequest::default(),
    )
    .await
    .expect_err("another dashboard's key must not be handed over");
    assert_eq!(grant.code, ErrorCode::FailedPrecondition);
}

/// All four methods are Device: a machine is not the operator whose stored
/// credential these are, and a key handed to a machine is a key on the fleet.
#[tokio::test]
async fn none_of_the_four_methods_answer_a_caller_that_is_not_a_browser() {
    let fixture = TranscriptionFixture::new("machine").await;
    let worker = fixture.worker();
    // The key is stored first, so each refusal is about the caller and not
    // about there being nothing to hand over.
    write_key(&fixture, &fixture.browser()).await;

    let refusals = [
        handle_transcription_get_config(
            &fixture.core,
            &worker,
            proto::TranscriptionGetConfigRequest::default(),
        )
        .await
        .map(|_| ()),
        handle_transcription_set_config(
            &fixture.core,
            &worker,
            set_request(Some(STORED_KEY), "en"),
        )
        .await
        .map(|_| ()),
        handle_transcription_grant_token(
            &fixture.core,
            &worker,
            proto::TranscriptionGrantTokenRequest::default(),
        )
        .await
        .map(|_| ()),
        handle_transcription_test(
            &fixture.core,
            &worker,
            proto::TranscriptionTestRequest::default(),
        )
        .await
        .map(|_| ()),
    ];

    for (index, refusal) in refusals.iter().enumerate() {
        let refusal = refusal.as_ref().expect_err("a machine is refused");
        assert_eq!(
            refusal.code,
            ErrorCode::Unauthenticated,
            "refusal {index} must be unauthenticated, not a permission fault"
        );
        assert!(
            refusal
                .response_headers()
                .contains_key(roost_coord::auth::principal::AUTH_LAYER_HEADER),
            "refusal {index} must carry the marker a client reads to re-pair"
        );
    }
}

/// Store [`STORED_KEY`], so a case is about what it names rather than the setup.
async fn write_key(
    fixture: &TranscriptionFixture,
    caller: &roost_coord::coord_core::Caller,
) -> proto::TranscriptionConfig {
    handle_transcription_set_config(&fixture.core, caller, set_request(Some(STORED_KEY), "en"))
        .await
        .expect("a key can be written")
        .body
}

/// A set request with the proto3 optional key present or absent.
fn set_request(key: Option<&str>, language: &str) -> proto::TranscriptionSetConfigRequest {
    proto::TranscriptionSetConfigRequest {
        deepgram_key: key.map(str::to_owned),
        deepgram_language: language.to_owned(),
        ..Default::default()
    }
}
