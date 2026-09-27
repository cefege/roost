//! The ceremony's own values: entropy, canonical validation, the stages a
//! request moves through, the two tab-scoped records, and the rule that a pair
//! request carries the public key and nothing else.
//!
//! The two properties here that have no visible symptom when they are wrong: a
//! verification code drawn by modulo is biased toward the low codes by 294 in
//! every 4.3 billion draws, which is a measurable edge on guessing a secret a
//! human reads aloud within a five-attempt limit; and a `PairCreate` that
//! echoes a request id this browser did not generate is treated as success,
//! which leaves a ceremony polling for a request that does not exist.

use roost_client_core::client::auth::{
    CeremonyStore, DeviceKeyManager, FixedRandomSource, KeyAdmission,
    MemorySecureKeyStore, PAIR_APPROVAL_STORAGE_KEY, PAIRING_CEREMONY_STORAGE_KEY,
    PAIRING_CEREMONY_VERSION, PairApproval, PairPollStatus, PairStage, PairingError,
    PairingSession, ScriptedRandomSource, compact_pair_verification_code,
    generate_pair_request_id, generate_pair_requester_token, generate_pair_verification_code,
    normalize_pair_request_id, normalize_pair_requester_token,
    normalize_pair_verification_code,
};
use roost_client_core::{MemoryClock, MemoryKeyValueStore};
// `get`/`set` are trait methods on `KeyValueStore`, not inherent on the
// in-memory store, so the trait has to be in scope for the tampered-record
// cases below to reach them.
use roost_client_core::KeyValueStore as _;

#[test]
fn ceremony_entropy_has_the_widths_the_wire_declares_and_one_spelling() {
    let source = FixedRandomSource::new(0xab);
    let request_id = generate_pair_request_id(&source).expect("request id");
    let token = generate_pair_requester_token(&source).expect("token");

    assert_eq!(request_id.len(), 32, "16 bytes, 32 lowercase hex characters");
    assert_eq!(token.len(), 64, "32 bytes, 64 lowercase hex characters");
    assert_eq!(request_id, "ab".repeat(16));
    assert_eq!(token, "ab".repeat(32));
    assert_eq!(normalize_pair_request_id(&request_id), Some(request_id.clone()));
    assert_eq!(
        normalize_pair_requester_token(&token),
        Some(token.clone())
    );
}

#[test]
fn a_value_with_a_second_spelling_is_not_the_value() {
    let id = "ab".repeat(16);
    // Uppercase is a different string, and a token-bound poll addressed in the
    // wrong case reads as a request that does not exist.
    assert_eq!(normalize_pair_request_id(&id.to_uppercase()), None);
    assert_eq!(normalize_pair_requester_token(&"AB".repeat(32)), None);
    assert_eq!(normalize_pair_request_id(&id[..31]), None, "one short");
    assert_eq!(normalize_pair_request_id(&format!("{id}0")), None, "one long");
    assert_eq!(normalize_pair_request_id(&"g".repeat(32)), None, "not hex");
}

#[test]
fn a_verification_code_is_six_digits_and_a_draw_above_the_limit_is_discarded() {
    // 0x0a0a0a0a = 168_430_090, which is inside the space, so the code is
    // 430090 — zero-padded, because a code that loses its leading zero is a
    // code the human cannot read back.
    let code = generate_pair_verification_code(&FixedRandomSource::new(0x0a)).expect("code");
    assert_eq!(code, "430090");
    assert_eq!(code.len(), 6);
    assert_eq!(normalize_pair_verification_code(&code), Some(code));

    // A draw at or above 4_294_000_000 is discarded and the NEXT draw is used:
    // 0x00010203 = 66_051 renders as 066051. Only a scripted source can state
    // this — the rejection window is the top 0.023% of the u32 range, and a
    // counting source's highest reachable draw is 0xff000102.
    let code =
        generate_pair_verification_code(&ScriptedRandomSource::rejecting_then(0x0001_0203))
            .expect("code");
    assert_eq!(code, "066051");

    // A code the human typed or pasted arrives with spaces in it often enough
    // that refusing it would fail a correct pairing. A dash is a different
    // value and is not quietly accepted.
    assert_eq!(compact_pair_verification_code(" 430 090 "), "430090");
    assert_eq!(
        normalize_pair_verification_code(&compact_pair_verification_code("430-090")),
        None
    );
    assert_eq!(normalize_pair_verification_code("43009"), None, "five digits");
    assert_eq!(normalize_pair_verification_code("4300900"), None, "seven");
}

#[test]
fn a_pair_request_carries_the_public_key_and_never_the_private_half() {
    let store = MemorySecureKeyStore::new();
    let flags = MemoryKeyValueStore::new();
    let clock = MemoryClock::new();
    let keys = DeviceKeyManager::new(&store, &flags, &NeverProbed, &clock);
    let session = PairingSession::create(&FixedRandomSource::new(0x11)).expect("ceremony");
    let request = session.create_request(&keys.public_key_b64().expect("public key"), "laptop");

    let key = store.current_key().expect("a key was minted");
    let private = store.private_bytes_for_test(key);
    let private_hex: String = private.iter().map(|byte| format!("{byte:02x}")).collect();
    let rendered = format!("{request:?}");
    assert!(
        !rendered.contains(private_hex.as_str()),
        "the request that leaves the origin must not carry the private key"
    );
    assert_eq!(request.ssh_pubkey_b64, keys.public_key_b64().expect("public key"));
    assert_eq!(request.label, "laptop");
    assert_eq!(request.ceremony_version, PAIRING_CEREMONY_VERSION);
    assert_eq!(
        request.ephemeral_id,
        session.ceremony().ephemeral_id,
        "the request names the ceremony that generated it"
    );
    assert_eq!(
        request.requester_token,
        session.ceremony().requester_token
    );
}

/// A probe that must never be reached; the manager's public key needs no probe.
struct NeverProbed;

impl roost_client_core::client::auth::DeviceKeyProbe for NeverProbed {
    fn probe_bearer(&self, _bearer: &str) -> KeyAdmission {
        KeyAdmission::Ambiguous
    }
}

#[test]
fn a_create_that_echoes_another_browsers_request_id_is_a_refusal() {
    let mut session = PairingSession::create(&FixedRandomSource::new(0x22)).expect("ceremony");
    let echoed = "cd".repeat(16);
    assert_eq!(
        session.on_create_response(&roost_client_core::client::auth::PairCreateResponse {
            ephemeral_id: echoed,
        }),
        Err(PairingError::RequestMismatch),
        "a matching echo is the only evidence the request exists"
    );
    assert_eq!(
        session.stage(),
        PairStage::Created,
        "a refused create must not advance the ceremony"
    );
    assert!(
        session.poll_request().is_none(),
        "and must not make the request pollable"
    );
}

#[test]
fn a_ceremony_moves_through_its_stages_and_refuses_the_ones_it_is_not_in() {
    let mut session = PairingSession::create(&FixedRandomSource::new(0x33)).expect("ceremony");
    let id = session.ceremony().ephemeral_id.clone();

    // Before the coordinator has the request there is nothing to poll and
    // nothing to confirm.
    assert!(session.poll_request().is_none());
    assert!(matches!(
        session.confirm_request("123456"),
        Err(PairingError::WrongStage { .. })
    ));

    session
        .on_create_response(&roost_client_core::client::auth::PairCreateResponse {
            ephemeral_id: id.clone(),
        })
        .expect("created");
    assert_eq!(session.stage(), PairStage::Acknowledged);
    let poll = session.poll_request().expect("pollable once created");
    assert_eq!(poll.ephemeral_id, id);

    // A poll that says nothing has happened leaves the stage alone.
    let status = session.on_poll_response(&roost_client_core::client::auth::PairPollResponse {
        status: "pending".to_string(),
        expires_at_ms: 600_000,
    });
    assert_eq!(status, PairPollStatus::Pending);
    assert_eq!(session.stage(), PairStage::Acknowledged);
    assert_eq!(session.expires_at_ms(), 600_000, "the expiry is remembered");

    // A status this client has never heard of stops the ceremony rather than
    // continuing to poll a state it cannot interpret.
    let status = session.on_poll_response(&roost_client_core::client::auth::PairPollResponse {
        status: "awaiting_notary".to_string(),
        expires_at_ms: 600_000,
    });
    assert_eq!(status, PairPollStatus::Unknown("awaiting_notary".to_string()));
    assert_eq!(status.as_wire(), None);
    assert!(!status.is_terminal());

    session.on_poll_response(&roost_client_core::client::auth::PairPollResponse {
        status: "verification_required".to_string(),
        expires_at_ms: 600_000,
    });
    assert_eq!(session.stage(), PairStage::VerificationRequired);

    // The code is compacted and validated before it can move the attempt
    // counter at the coordinator.
    assert_eq!(
        session.confirm_request(" 12 34 5a "),
        Err(PairingError::MalformedVerificationCode)
    );
    let confirm = session.confirm_request(" 123 456 ").expect("confirmable");
    assert_eq!(confirm.verification_code, "123456");
    assert_eq!(confirm.ephemeral_id, id);

    // A wrong code is an answer, not a failure: the ceremony stays live.
    session.on_confirm_response(&roost_client_core::client::auth::PairConfirmResponse { ok: false });
    assert_eq!(session.stage(), PairStage::VerificationRequired);
    session.on_confirm_response(&roost_client_core::client::auth::PairConfirmResponse { ok: true });
    assert_eq!(session.stage(), PairStage::Completed);

    // A completed ceremony is terminal, and nothing may be confirmed into it.
    let terminal = PairPollStatus::Completed;
    assert!(terminal.is_terminal());
    assert!(matches!(
        session.confirm_request("123456"),
        Err(PairingError::WrongStage { .. })
    ));
    assert!(session.poll_request().is_none());
}

#[test]
fn every_terminal_status_ends_the_ceremony() {
    for (wire, expected) in [
        ("denied", PairPollStatus::Denied),
        ("expired", PairPollStatus::Expired),
        ("verification_failed", PairPollStatus::VerificationFailed),
        ("completed", PairPollStatus::Completed),
    ] {
        let mut session =
            PairingSession::create(&FixedRandomSource::new(0x44)).expect("ceremony");
        let id = session.ceremony().ephemeral_id.clone();
        session
            .on_create_response(&roost_client_core::client::auth::PairCreateResponse {
                ephemeral_id: id,
            })
            .expect("created");
        let status = session.on_poll_response(
            &roost_client_core::client::auth::PairPollResponse {
                status: wire.to_string(),
                expires_at_ms: 1,
            },
        );
        assert_eq!(status, expected);
        assert_eq!(status.as_wire(), Some(wire));
        assert!(status.is_terminal(), "{wire} must end the ceremony");
        if wire != "completed" {
            assert_eq!(session.stage(), PairStage::Terminal);
        }
    }
}

#[test]
fn an_approver_binds_the_code_it_generated_and_keeps_it_across_a_reload() {
    let request_id = "ef".repeat(16);
    let approval = PairApproval::generate(
        &FixedRandomSource::new(0x0a),
        &request_id,
        "mike's laptop",
        600_000,
    )
    .expect("approval");

    let approve = approval.approve_request();
    assert_eq!(approve.ephemeral_id, request_id);
    assert_eq!(approve.verification_code, "430090");
    assert_eq!(approve.ceremony_version, PAIRING_CEREMONY_VERSION);
    assert_eq!(approval.deny_request().ephemeral_id, request_id);
    assert_eq!(
        approval.status_request().ephemeral_id,
        request_id,
        "the approver's own progress lookup names the same request"
    );

    // A bad request id is refused at generation rather than becoming an
    // approval the coordinator will reject. The VARIANT is the point:
    // `MalformedRequestId` is its own case precisely because the id and the
    // code arrive from different people, and a bare `is_err()` would be equally
    // satisfied by an entropy failure.
    assert!(matches!(
        PairApproval::generate(&FixedRandomSource::new(0x0a), "not-an-id", "laptop", 1),
        Err(PairingError::MalformedRequestId)
    ));
}

#[test]
fn the_ceremonys_records_survive_a_reload_and_a_tampered_one_is_deleted() {
    let storage = MemoryKeyValueStore::new();
    let store = CeremonyStore::new(&storage);

    let session = PairingSession::create(&FixedRandomSource::new(0x55)).expect("ceremony");
    let ceremony = session.ceremony().clone();
    store.save_ceremony(&ceremony);
    let restored = store.load_ceremony().expect("the capability survives");
    assert_eq!(restored, ceremony);
    assert_eq!(
        PairingSession::restore(restored).map(|s| s.ceremony().clone()),
        Some(ceremony.clone()),
        "and a restored ceremony is one this client will act on"
    );

    // The approver's code is persisted for a different reason: an approval
    // interrupted by a reload must bind the code the human was already told.
    let approval = PairApproval::generate(
        &FixedRandomSource::new(0x0a),
        &ceremony.ephemeral_id,
        "laptop",
        600_000,
    )
    .expect("approval");
    store.save_approval(&approval);
    assert_eq!(store.load_approval(), Some(approval.clone()));
    assert_eq!(
        store.load_approval().expect("approval").approve_request(),
        approval.approve_request(),
        "the reloaded approval binds the same code"
    );

    store.clear_ceremony();
    store.clear_approval();
    assert!(store.load_ceremony().is_none());
    assert!(store.load_approval().is_none());

    // A record this client did not write, or one from another ceremony
    // version, is removed on read rather than half-trusted: the requester token
    // is the only thing that can finish a pairing, and a half-understood one
    // produces polls that can only ever be refused.
    for tampered in [
        "not json at all",
        "{}",
        r#"{"ceremonyVersion":1,"ephemeralId":"ef01"}"#,
        r#"{"ceremonyVersion":2,"ephemeralId":"ef01","requesterToken":"ab"}"#,
        r#"{"ceremonyVersion":1,"ephemeralId":"nothex!!","requesterToken":"ab"}"#,
        r#"{"ceremonyVersion":1,"ephemeralId":"ef01","requesterToken":"ab","extra":1}"#,
    ] {
        storage.set(PAIRING_CEREMONY_STORAGE_KEY, tampered);
        assert!(
            store.load_ceremony().is_none(),
            "{tampered} must not be adopted"
        );
        assert!(
            storage.get(PAIRING_CEREMONY_STORAGE_KEY).is_none(),
            "{tampered} must also be deleted"
        );
    }

    // The approver record holds a code and a deadline, and a deadline of zero
    // would make an approval that can never expire.
    storage.set(
        PAIR_APPROVAL_STORAGE_KEY,
        r#"{"ceremonyVersion":1,"ephemeralId":"ef01","verificationCode":"430090","requesterLabel":"laptop","expiresAtMs":0}"#,
    );
    assert!(store.load_approval().is_none());
    assert!(storage.get(PAIR_APPROVAL_STORAGE_KEY).is_none());
}
