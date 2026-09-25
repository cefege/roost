//! The Tier-1 vocabulary is a contract with `roost doctor` and with an
//! operator's `grep`, so these tests pin the shape of the whole table and the
//! exact spelling of the entries whose shape is easy to get wrong.

use std::collections::BTreeSet;

use roost_observability::SignalKind;

#[test]
fn every_kind_round_trips_through_its_literal() {
    for kind in SignalKind::ALL {
        assert_eq!(SignalKind::from_name(kind.as_str()), Some(kind));
        assert_eq!(kind.to_string(), kind.as_str());
    }
}

#[test]
fn no_two_kinds_share_a_literal() {
    // Two kinds on one token would share one cooldown scope, so a repeat of
    // the first would silence the second.
    let mut seen = BTreeSet::new();
    for kind in SignalKind::ALL {
        assert!(
            seen.insert(kind.as_str()),
            "duplicate literal {}",
            kind.as_str()
        );
    }
    assert_eq!(seen.len(), SignalKind::ALL.len());
}

#[test]
fn the_list_covers_every_kind_exactly_once() {
    assert_eq!(SignalKind::ALL.len(), 73);
    let mut listed: Vec<SignalKind> = SignalKind::ALL.to_vec();
    listed.sort_unstable();
    listed.dedup();
    assert_eq!(listed.len(), SignalKind::ALL.len());
}

#[test]
fn an_unrecognized_token_is_not_guessed_at() {
    assert_eq!(SignalKind::from_name("not.a.kind"), None);
    assert_eq!(SignalKind::from_name(""), None);
    assert_eq!(SignalKind::from_name("Keeper.Died"), None, "case matters");
}

#[test]
fn the_awkward_spellings_are_exact() {
    // Digits, a hyphen and the two-word names are where a rename slips in.
    for (kind, literal) in [
        (SignalKind::AuthRelogin401, "auth.relogin_401"),
        (SignalKind::CfAccessRejected, "cf-access.rejected"),
        (SignalKind::TerminalCaptureSaved, "terminal.capture_saved"),
        (SignalKind::AuthJwtSignFail, "auth.jwt_sign_fail"),
        (SignalKind::DiagCorruptionSignal, "diag.corruption_signal"),
        (
            SignalKind::KeeperDegradedUnrecoverable,
            "keeper.degraded_unrecoverable",
        ),
        (
            SignalKind::AuditInputQueueBackpressure,
            "audit.input_queue_backpressure",
        ),
        (SignalKind::SyncWsFrameDropped, "sync.ws_frame_dropped"),
    ] {
        assert_eq!(kind.as_str(), literal);
    }
}

#[test]
fn every_literal_is_a_lowercase_namespace_token() {
    for kind in SignalKind::ALL {
        let literal = kind.as_str();
        let dots = literal.matches('.').count();
        assert!((1..=2).contains(&dots), "{literal} has {dots} dots");
        assert!(
            literal.bytes().all(|byte| byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || b"._-".contains(&byte)),
            "{literal} is not a snake_case namespace token"
        );
    }
}
