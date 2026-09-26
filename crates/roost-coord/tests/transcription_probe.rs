//! The provider probe's lifecycle, which is the only work in this domain a
//! third party can hold open.
//!
//! The property under test is that a probe always settles. A test that waited
//! on the real provider would prove nothing about whether the wait is bounded,
//! so every provider here is installed by the test, and the cases are the three
//! outcomes a caller has to tell apart: the provider accepted the key, the
//! provider refused it, and the provider never answered.

// A test that cannot say what it expected is not a test. `expect` is denied
// outside `#[cfg(test)]`, and an integration test is its own crate, so the
// exemption has to be stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod transcription_fixture;

use std::sync::Arc;

use roost_coord::coord_core::Caller;
use roost_coord::diagnostics::rpc_transcription::{
    handle_transcription_set_config, handle_transcription_test,
};
use roost_coord::diagnostics::transcription::{
    PROBE_DEADLINE, ProbeFuture, ProbeSender, ProviderProbe,
};
use roost_proto as proto;
use tokio::sync::Notify;
use transcription_fixture::{
    STORED_KEY, TranscriptionFixture, accepting, never_answering, refusing,
};

/// The reason the refusing providers below answer with.
const REFUSED: &str = "Key rejected by Deepgram (401)";

/// A provider that accepts the key settles as reachable and reports no error.
#[tokio::test]
async fn a_provider_that_accepts_the_key_settles_as_reachable() {
    let fixture = TranscriptionFixture::new("accepts").await;
    store_key(&fixture).await;
    fixture.probe_with(accepting());

    let test = test(&fixture).await;

    assert!(test.ok, "the provider accepted the key");
    assert_eq!(test.error, "", "a success carries no error line");
    let settled = fixture.probe_state();
    assert!(
        matches!(settled, ProviderProbe::Reachable { .. }),
        "the settled state is reachable, got {settled:?}"
    );
}

/// A provider that refuses reaches a terminal failure carrying its reason.
///
/// Not a probe left pending: the reason is the whole point of the method, and a
/// caller that saw neither would have to guess whether the button did anything.
#[tokio::test]
async fn a_provider_that_refuses_reaches_a_terminal_failure_carrying_its_reason() {
    let fixture = TranscriptionFixture::new("refuses").await;
    store_key(&fixture).await;
    fixture.probe_with(refusing(REFUSED));

    let test = test(&fixture).await;

    assert!(!test.ok);
    assert_eq!(test.error, REFUSED, "the provider's own words, not a summary");

    let settled = fixture.probe_state();
    assert!(
        settled.is_terminal(),
        "a refused probe must settle, not stay pending: {settled:?}"
    );
    assert_eq!(
        settled.failure_reason().as_deref(),
        Some(REFUSED),
        "the settled failure carries the provider's reason"
    );
    assert!(
        !matches!(settled, ProviderProbe::Unanswered { .. }),
        "a provider that answered did not go unanswered"
    );
}

/// A provider that never answers is a different fact from one that said no.
///
/// This is the case the deadline exists for, and it is why the bound is a
/// named constant rather than a client's patience: the two failures send the
/// operator somewhere different, so they cannot share a state.
#[tokio::test]
async fn a_provider_that_never_answers_settles_as_unanswered_rather_than_pending() {
    let fixture = TranscriptionFixture::new("silent").await;
    store_key(&fixture).await;
    fixture.probe_with(never_answering());

    let test = test(&fixture).await;

    assert!(!test.ok);
    match fixture.probe_state() {
        ProviderProbe::Unanswered {
            after_ms,
            finished_ms,
        } => {
            assert_eq!(
                after_ms,
                deadline_ms(),
                "the reported bound is the one the runtime is using"
            );
            assert!(finished_ms >= 0, "the deadline was observed at an instant");
        }
        other => panic!("a silent provider settles as unanswered, got {other:?}"),
    }
    assert_eq!(
        test.error,
        format!("Deepgram did not answer within {}ms", deadline_ms()),
        "the operator is told the wait ended, not that the key is bad"
    );
}

/// While the provider has not answered the probe reads as pending, and only
/// then -- which is what makes a settled state worth reading at all.
#[tokio::test]
async fn a_probe_is_pending_while_the_provider_has_not_answered() {
    let fixture = TranscriptionFixture::new("in-flight").await;
    store_key(&fixture).await;
    let gate = Arc::new(Gate::default());
    fixture.probe_with(gated(gate.clone()));

    let caller = fixture.browser();
    let core = fixture.core.clone();
    let in_flight = tokio::spawn(async move {
        handle_transcription_test(
            &core,
            &caller,
            proto::TranscriptionTestRequest::default(),
        )
        .await
    });
    // The provider announces that it was called; until it does, nothing below
    // can tell a probe that started from one that never will.
    gate.started.notified().await;

    let pending = fixture.probe_state();
    assert!(
        matches!(pending, ProviderProbe::Pending { .. }),
        "a probe in flight reads as pending, got {pending:?}"
    );
    assert!(!pending.is_terminal(), "a probe in flight is not a failure");
    assert_eq!(
        pending.failure_reason(),
        None,
        "a probe in flight has no reason to report yet"
    );

    gate.answered.notify_one();
    let test = in_flight
        .await
        .expect("the probe task")
        .expect("a test is answered")
        .body;
    assert!(test.ok, "the provider accepted once it answered");
    let settled = fixture.probe_state();
    assert!(
        matches!(settled, ProviderProbe::Reachable { .. }),
        "an answered probe settles, got {settled:?}"
    );
}

/// A coordinator with no key saved does not call the provider at all.
#[tokio::test]
async fn a_config_with_no_key_reports_the_missing_key_without_calling_the_provider() {
    let fixture = TranscriptionFixture::new("nothing-to-test").await;
    fixture.probe_with(accepting());

    let test = test(&fixture).await;

    assert!(!test.ok);
    assert_eq!(test.error, "No Deepgram key saved");
    assert_eq!(
        fixture.probe_state(),
        ProviderProbe::Idle,
        "no provider was called, so no probe was started"
    );
}

/// Clearing the key stops the next test reaching the provider, and leaves the
/// last real probe as what the process reports.
///
/// The alternative -- forgetting the refusal once the key is gone -- would make
/// the state read as though the deployment had never been tested.
#[tokio::test]
async fn a_cleared_key_starts_no_new_probe_and_keeps_the_last_one_reported() {
    let fixture = TranscriptionFixture::new("cleared").await;
    let caller = fixture.browser();
    store_key(&fixture).await;
    fixture.probe_with(refusing(REFUSED));
    assert!(!test(&fixture).await.ok);

    handle_transcription_set_config(
        &fixture.core,
        &caller,
        proto::TranscriptionSetConfigRequest {
            deepgram_key: Some(String::new()),
            deepgram_language: "en".to_owned(),
        },
    )
    .await
    .expect("a key can be cleared");

    let after_clear = test(&fixture).await;
    assert_eq!(
        after_clear.error, "No Deepgram key saved",
        "a cleared key is not tested against the provider"
    );
    assert_eq!(
        fixture.probe_state().failure_reason().as_deref(),
        Some(REFUSED),
        "the last real probe is still what the process reports"
    );
}

/// One `TranscriptionTest` as the browser makes it.
async fn test(fixture: &TranscriptionFixture) -> proto::TranscriptionTestResponse {
    handle_transcription_test(
        &fixture.core,
        &fixture.browser(),
        proto::TranscriptionTestRequest::default(),
    )
    .await
    .expect("a test is answered")
    .body
}

/// Store the key the tests probe with.
async fn store_key(fixture: &TranscriptionFixture) {
    handle_transcription_set_config(
        &fixture.core,
        &fixture.browser(),
        proto::TranscriptionSetConfigRequest {
            deepgram_key: Some(STORED_KEY.to_owned()),
            deepgram_language: "en".to_owned(),
        },
    )
    .await
    .expect("a key can be written");
}

/// The bound as a whole number of milliseconds, which is what the state carries.
fn deadline_ms() -> i64 {
    i64::try_from(PROBE_DEADLINE.as_millis()).expect("a ten second bound fits an i64")
}

/// The two ends of a probe the test holds open.
#[derive(Default)]
struct Gate {
    /// The provider has been called.
    started: Notify,
    /// The provider may answer.
    answered: Notify,
}

/// A probe sender that announces itself and then parks until the test answers.
fn gated(gate: Arc<Gate>) -> ProbeSender {
    Arc::new(move |_key: String| -> ProbeFuture {
        let gate = Arc::clone(&gate);
        Box::pin(async move {
            gate.started.notify_one();
            gate.answered.notified().await;
            Ok(())
        })
    })
}
