//! The reuse window for a signed coordinator JWT: one Ed25519 signature per
//! [`JWT_CACHE_TTL_MS`], not one per request. Owned by `platform::device_key`,
//! whose `bearer` consults it before signing. Target-independent and natively
//! tested; the window constant is the client core's. Ported from the
//! `_cachedJwt` rule in `apps/web/src/client/auth/web-key.ts`.

use roost_client_core::client::auth::jwt::{CoordinatorJwt, JWT_CACHE_TTL_MS};

/// The last token this device signed, and the millisecond it was signed at.
#[derive(Debug, Default)]
pub struct BearerCache {
    signed: Option<(CoordinatorJwt, u64)>,
}

impl BearerCache {
    /// The cached token, if `now_ms` is still inside its reuse window.
    ///
    /// v2's `age >= 0 && age < JWT_CACHE_TTL_MS`: a clock that stepped
    /// backwards past the signing instant re-signs rather than serving a token
    /// whose `iat` lies in the future, and a token exactly one window old is
    /// re-signed because it has only a minute of `exp` left.
    pub fn reusable(&self, now_ms: u64) -> Option<&str> {
        let (token, signed_at_ms) = self.signed.as_ref()?;
        let age_ms = now_ms.checked_sub(*signed_at_ms)?;
        (age_ms < JWT_CACHE_TTL_MS).then(|| token.token())
    }

    /// Remember a freshly signed token as the one to reuse.
    pub fn store(&mut self, token: CoordinatorJwt, signed_at_ms: u64) {
        self.signed = Some((token, signed_at_ms));
    }
}

#[cfg(test)]
mod tests {
    use roost_client_core::client::auth::jwt::{
        CoordinatorJwt, JWT_CACHE_TTL_MS, build_unsigned_jwt,
    };

    use super::BearerCache;

    const SIGNED_AT_MS: u64 = 1_783_728_000_000;

    fn cache_signed_at(signed_at_ms: u64) -> BearerCache {
        let unsigned = build_unsigned_jwt(&"a".repeat(64), signed_at_ms);
        let mut cache = BearerCache::default();
        cache.store(CoordinatorJwt::mint(&unsigned, &[7; 64], signed_at_ms), signed_at_ms);
        cache
    }

    #[test]
    fn an_empty_cache_has_nothing_to_reuse() {
        assert_eq!(BearerCache::default().reusable(SIGNED_AT_MS), None);
    }

    #[test]
    fn a_token_is_reused_up_to_the_last_millisecond_of_its_window() {
        let cache = cache_signed_at(SIGNED_AT_MS);
        assert!(cache.reusable(SIGNED_AT_MS).is_some());
        assert!(cache.reusable(SIGNED_AT_MS + JWT_CACHE_TTL_MS - 1).is_some());
    }

    #[test]
    fn a_token_exactly_one_window_old_is_not_reused() {
        let cache = cache_signed_at(SIGNED_AT_MS);
        assert_eq!(cache.reusable(SIGNED_AT_MS + JWT_CACHE_TTL_MS), None);
    }

    #[test]
    fn a_clock_that_stepped_backwards_is_not_served_the_cached_token() {
        let cache = cache_signed_at(SIGNED_AT_MS);
        assert_eq!(cache.reusable(SIGNED_AT_MS - 1), None);
    }
}
