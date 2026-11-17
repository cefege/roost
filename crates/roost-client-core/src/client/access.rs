//! Read-only connection facts for onboarding, settings and outage recovery.
//! Hosts supply browser reachability; the store remains the authorization owner.
//! Endpoint selection is shared with bootstrap, never inferred from saved UI state.

pub mod enrollment;

use super::local::bootstrap::{DEPLOYMENT_MODE_SELF_HOSTED, LocalBootstrap};
use crate::store::{BrowserAccessState, Store};
use crate::sync::redial::sync_link_answering;

/// The authority that selected this document's coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinatorSource {
    DocumentOrigin,
    ServingWorker,
    StoredOverride,
}

impl CoordinatorSource {
    pub const fn description(self) -> &'static str {
        match self {
            Self::DocumentOrigin => "The server that opened this page",
            Self::ServingWorker => "Advertised by this computer's worker",
            Self::StoredOverride => "Your explicit self-hosted connection",
        }
    }
}

/// Borrowed selection: resolving an endpoint performs no allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoordinatorSelection<'a> {
    pub origin: &'a str,
    pub source: CoordinatorSource,
}

/// Resolve transport and presentation through the same precedence.
pub fn select_coordinator<'a>(
    served_by_worker: Option<&'a LocalBootstrap>,
    deployment_mode: Option<&str>,
    stored_override: Option<&'a str>,
    page_origin: &'a str,
) -> CoordinatorSelection<'a> {
    if let Some(bootstrap) = served_by_worker {
        return CoordinatorSelection {
            origin: &bootstrap.coordinator_url,
            source: CoordinatorSource::ServingWorker,
        };
    }
    if deployment_mode == Some(DEPLOYMENT_MODE_SELF_HOSTED)
        && let Some(origin) = stored_override
            .map(str::trim)
            .filter(|origin| enrollment::browser_origin_is_valid(origin))
    {
        return CoordinatorSelection {
            origin,
            source: CoordinatorSource::StoredOverride,
        };
    }
    CoordinatorSelection {
        origin: page_origin,
        source: CoordinatorSource::DocumentOrigin,
    }
}

/// Reachability does not imply device authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinatorReachability {
    BrowserOffline,
    Answering,
    Waiting,
}

/// Safe connection facts; no protected fleet records or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessOverview<'a> {
    pub coordinator: CoordinatorSelection<'a>,
    pub document_origin: &'a str,
    pub declared_public_url: Option<&'a str>,
    pub authorization: BrowserAccessState,
    pub reachability: CoordinatorReachability,
    pub local_worker_origin: Option<&'a str>,
}

impl<'a> AccessOverview<'a> {
    pub fn read(
        store: &'a Store,
        coordinator: CoordinatorSelection<'a>,
        document_origin: &'a str,
        browser_online: bool,
        now_ms: u64,
        local_worker_origin: Option<&'a str>,
    ) -> Self {
        let reachability = if !browser_online {
            CoordinatorReachability::BrowserOffline
        } else if sync_link_answering(store.sync.idle_ms(now_ms)) {
            CoordinatorReachability::Answering
        } else {
            CoordinatorReachability::Waiting
        };
        Self {
            coordinator,
            document_origin,
            declared_public_url: store
                .coord_identity
                .as_ref()
                .map(|identity| identity.public_url.as_str()),
            authorization: store.browser_access_state,
            reachability,
            local_worker_origin,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CoordinatorSource, select_coordinator};
    use crate::client::local::bootstrap::{DEPLOYMENT_MODE_SELF_HOSTED, LocalBootstrap};

    #[test]
    fn saved_preferences_cannot_claim_to_be_the_effective_connection() {
        let selected =
            select_coordinator(None, None, Some("https://other.test"), "https://page.test");
        assert_eq!(selected.origin, "https://page.test");
        assert_eq!(selected.source, CoordinatorSource::DocumentOrigin);
        let selected = select_coordinator(
            None,
            Some(DEPLOYMENT_MODE_SELF_HOSTED),
            Some("https://other.test"),
            "https://page.test",
        );
        assert_eq!(selected.origin, "https://other.test");
        assert_eq!(selected.source, CoordinatorSource::StoredOverride);
    }

    #[test]
    fn worker_bootstrap_wins_over_an_explicit_saved_choice() {
        let bootstrap = LocalBootstrap {
            coordinator_url: "https://worker-coordinator.test".to_owned(),
            worker_fingerprint: "worker".to_owned(),
        };
        let selected = select_coordinator(
            Some(&bootstrap),
            Some(DEPLOYMENT_MODE_SELF_HOSTED),
            Some("https://other.test"),
            "http://127.0.0.1:4114",
        );
        assert_eq!(selected.origin, bootstrap.coordinator_url);
        assert_eq!(selected.source, CoordinatorSource::ServingWorker);
    }
}

#[cfg(test)]
mod override_tests {
    use super::{CoordinatorSource, select_coordinator};
    use crate::client::local::bootstrap::DEPLOYMENT_MODE_SELF_HOSTED;

    #[test]
    fn credential_bearing_legacy_override_cannot_be_used_or_copied_as_an_origin() {
        for invalid in [
            "https://roost.test?pair=secret",
            "https://user:secret@roost.test",
            "http://127.attacker.test",
        ] {
            let selected = select_coordinator(
                None,
                Some(DEPLOYMENT_MODE_SELF_HOSTED),
                Some(invalid),
                "https://page.test",
            );
            assert_eq!(selected.origin, "https://page.test");
            assert_eq!(selected.source, CoordinatorSource::DocumentOrigin);
        }
    }
}
