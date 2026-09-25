//! A branded id must be unreachable from a decoder that never ran its check:
//! the nominal type is only worth something if the shape is enforced on the
//! wire path, not only at the constructor.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::wire::brand::{SessionId, WorkerFp};

const SESSION: &str = "00000000-0000-4000-8000-000000000001";

#[test]
fn a_decoder_cannot_mint_a_brand_that_did_not_pass_its_check() {
    let forged: Result<SessionId, _> = serde_json::from_str("\"not-a-uuid\"");
    assert!(forged.is_err());
    let honest: SessionId =
        serde_json::from_str("\"00000000-0000-4000-8000-000000000001\"").expect("a uuid");
    assert_eq!(honest.as_str(), SESSION);
    let short_fingerprint: Result<WorkerFp, _> = serde_json::from_str("\"abab\"");
    assert!(short_fingerprint.is_err());
    let uppercased: Result<WorkerFp, _> = serde_json::from_str(&format!("\"{}\"", "A".repeat(64)));
    assert!(uppercased.is_err());
}
