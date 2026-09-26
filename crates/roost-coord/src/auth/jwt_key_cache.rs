//! The process's cache of authorized public keys, and the generations that
//! make a revocation win a race.
//!
//! Owned by the coordinator's auth layer. `jwt_verify` reads this; the
//! authorized-keys loader in `authorized_keys` fills it. Constructed once per
//! process and injected, never a crate-root static, because boot builds it
//! before any transport exists (`apps/coord/src/main.ts:87`).
//!
//! THE RACE THIS EXISTS FOR. Verification is four steps and the key can be
//! revoked between any two of them: look the `kid` up, verify the signature,
//! validate the claims, resolve the principal. Without a generation, a socket
//! that started verifying against a live key finishes minting a principal for a
//! key revoked in the middle, and that principal then outlives the revocation on
//! every long-lived socket. v2 re-checks the generation at **four** points --
//! before and after key import, after signature verification, and after claim
//! validation (`apps/coord/src/auth/jwt.ts:108,117,181,222`). A port that checks
//! it once has a smaller hole; a port that never checks has the original one.
//! This file makes the counter cheap so checking it four times costs nothing.
//!
//! WHY `refresh` AND `invalidate` DIFFER. `invalidate_jwt_key` bumps the
//! generation AND drops the cached row; `refresh_jwt_key` drops the row only
//! (`apps/coord/src/auth/jwt.ts:79-89`). Refresh runs after mint, redeem and
//! pair -- the key is still authorized, so a verifier that already loaded the row
//! must stay valid. Invalidate runs on revoke, rotate and logout. Conflating them
//! breaks enrollment (a new key never becomes usable) or revocation (an old key
//! never stops working); both failures are silent.
//!
//! WHY THE METHODS TAKE `&self`. The verification path is `async` and holds this
//! cache across an await, so the interior `Mutex` is what lets a `&JwtKeyCache`
//! cross that boundary. The lock is never held across an await -- each method
//! takes and drops it -- so the only contention is between concurrent token
//! verifications, which is a few nanoseconds of map access.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::jwt_crypto::PublicKey;

/// How long a cached key stays before it is re-read from the database.
///
/// 60 s (`apps/coord/src/auth/jwt.ts:10`). Short enough that a direct edit to
/// `authorized_keys` is picked up on its own, long enough that a keystroke-rate
/// Connect call does not read a row per request.
pub const CACHE_TTL: Duration = Duration::from_secs(60);

/// A cached authorized key.
///
/// It deliberately does NOT store the generation it was loaded under. A caller
/// that read the row has already consulted the generation, and
/// [`JwtKeyCache::generation_is_current`] is re-checked at each step of every
/// verification; a second copy here would imply a weaker path that skips the
/// database read on a stale row.
#[derive(Debug, Clone)]
struct CachedKey {
    key: PublicKey,
    label: String,
    loaded_at: Instant,
}

/// Whether a cached row may be used as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFreshness {
    /// The row was cached within [`CACHE_TTL`].
    Fresh,
    /// The row is older than [`CACHE_TTL`] and must be re-read.
    Stale,
}

/// Per-fingerprint authorized keys and their revocation generations.
///
/// `Debug` is derived because the workspace warns on a missing `Debug`; what it
/// holds is a key *identifier* and a public half, never key material.
#[derive(Debug, Default)]
pub struct JwtKeyCache {
    inner: Mutex<CacheInner>,
}

#[derive(Debug, Default)]
struct CacheInner {
    keys: HashMap<String, CachedKey>,
    generations: HashMap<String, u64>,
}

impl JwtKeyCache {
    /// An empty cache. Every generation starts at zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn with_inner<R>(&self, body: impl FnOnce(&mut CacheInner) -> R) -> R {
        // A poisoned lock means some other thread panicked while holding it. The
        // map is a plain cache with no invariant a panic could leave broken, so
        // the cache is rebuilt rather than propagating a panic into every
        // subsequent request.
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        body(&mut guard)
    }

    /// The current generation for a `kid`, or zero if it has never been seen.
    ///
    /// Zero is the answer for an unknown key, which is why a generation check is
    /// not authorization on its own: an unknown key has a perfectly valid
    /// generation and still must not authenticate.
    #[must_use]
    pub fn generation(&self, kid: &str) -> u64 {
        self.with_inner(|inner| inner.generations.get(kid).copied().unwrap_or(0))
    }

    /// The cached key, and whether it is still inside its TTL.
    #[must_use]
    pub fn lookup(&self, kid: &str, now: Instant) -> Option<(PublicKey, String, KeyFreshness)> {
        self.with_inner(|inner| {
            let entry = inner.keys.get(kid)?;
            let freshness = if now.duration_since(entry.loaded_at) < CACHE_TTL {
                KeyFreshness::Fresh
            } else {
                KeyFreshness::Stale
            };
            Some((entry.key, entry.label.clone(), freshness))
        })
    }

    /// Record a freshly read key.
    ///
    /// A caller that read the row has already consulted the generation, and
    /// `generation_is_current` is re-checked at each verification step, so the
    /// cache deliberately stores no generation of its own: a second copy would
    /// imply a weaker path that skips the database read on a stale row.
    pub fn store(&self, kid: &str, key: PublicKey, label: &str, now: Instant) {
        self.with_inner(|inner| {
            inner.keys.insert(
                kid.to_string(),
                CachedKey {
                    key,
                    label: label.to_string(),
                    loaded_at: now,
                },
            );
        });
    }

    /// Drop the cached row but leave the generation alone.
    ///
    /// For mint, redeem and pair: the key is still authorized and a verifier that
    /// already loaded its row must stay valid.
    pub fn refresh_jwt_key(&self, kid: &str) {
        self.with_inner(|inner| {
            inner.keys.remove(kid);
        });
    }

    /// Bump the generation and drop the cached row.
    ///
    /// For revoke, rotate and logout. The bump is what makes an in-flight
    /// verification that already read the row fail its next generation check.
    pub fn invalidate_jwt_key(&self, kid: &str) {
        self.with_inner(|inner| {
            let next = inner
                .generations
                .get(kid)
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            inner.generations.insert(kid.to_string(), next);
            inner.keys.remove(kid);
        });
    }

    /// Drop every key and generation for a fingerprint.
    ///
    /// For worker delete, where the key row survives as a tombstone but must
    /// stop being usable immediately rather than at the next TTL expiry.
    pub fn forget_worker(&self, kid: &str) {
        self.invalidate_jwt_key(kid);
    }

    /// Whether a generation observed earlier is still current.
    ///
    /// All four check sites of `apps/coord/src/auth/jwt.ts` reduce to this.
    #[must_use]
    pub fn generation_is_current(&self, kid: &str, observed: u64) -> bool {
        self.generation(kid) == observed
    }
}
