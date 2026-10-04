//! The connection banner's verdict: which outage it names, in which order, and
//! that a quiet but open Sync link is not an outage. Native, because the
//! verdict is a pure function of one reading of the browser and the store.

use roost_client_core::sync::redial::SYNC_STALE_TIMEOUT_MS;
use roost_web::components::notifications::connection_banner::{
    BannerInputs, BannerReason, banner_verdict,
};

fn healthy() -> BannerInputs {
    BannerInputs {
        auth_revoked: false,
        browser_online: true,
        link_idle_ms: Some(0),
        direct_live: false,
    }
}

#[test]
fn an_open_link_quiet_between_keepalives_raises_no_banner() {
    // An idle coordinator speaks once per 30 s keepalive, and one can land late.
    let verdict = banner_verdict(BannerInputs {
        link_idle_ms: Some(35_000),
        ..healthy()
    });
    assert_eq!(verdict, None);
}

#[test]
fn no_open_socket_is_an_outage_named_for_what_still_works() {
    let closed = BannerInputs {
        link_idle_ms: None,
        ..healthy()
    };
    assert_eq!(banner_verdict(closed), Some(BannerReason::CoordUnreachable));
    assert_eq!(
        banner_verdict(BannerInputs {
            direct_live: true,
            ..closed
        }),
        Some(BannerReason::CoordUnreachableDirectLive)
    );
}

#[test]
fn a_link_silent_for_the_watchdog_bound_is_an_outage() {
    let verdict = banner_verdict(BannerInputs {
        link_idle_ms: Some(SYNC_STALE_TIMEOUT_MS),
        ..healthy()
    });
    assert_eq!(verdict, Some(BannerReason::CoordUnreachable));
    let verdict = banner_verdict(BannerInputs {
        link_idle_ms: Some(SYNC_STALE_TIMEOUT_MS - 1),
        ..healthy()
    });
    assert_eq!(verdict, None);
}

#[test]
fn a_revoked_credential_outranks_offline_and_offline_outranks_the_coordinator() {
    let everything_wrong = BannerInputs {
        auth_revoked: true,
        browser_online: false,
        link_idle_ms: None,
        direct_live: true,
    };
    assert_eq!(
        banner_verdict(everything_wrong),
        Some(BannerReason::AuthRevoked)
    );
    assert_eq!(
        banner_verdict(BannerInputs {
            auth_revoked: false,
            ..everything_wrong
        }),
        Some(BannerReason::Offline)
    );
}
