//! The gathering inputs that outlive one offer: this host's offerable
//! addresses and the resolved STUN server addresses, each reused for
//! [`GATHER_CACHE_TTL`]. Owned by `peer::native::factory` and handed to every
//! peer it creates; read by `peer::native::gather` on each answer. Depends on
//! `host_addresses` and `gather::resolve_servers` for a miss.

use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::gather::resolve_servers;
use super::host_addresses;

/// How long an answer is reused. Long enough that a burst of peers (one per
/// browser tab) pays the interface walk and the DNS lookup once; short enough
/// that a changed network is seen within a minute.
pub(super) const GATHER_CACHE_TTL: Duration = Duration::from_secs(60);

/// Resolved servers, remembered with the URL list they answer for.
#[derive(Debug, Clone)]
struct ResolvedServers {
    stun_urls: Vec<String>,
    servers: Vec<SocketAddr>,
}

/// One cached answer per input. A lock is never held across an await: the
/// cache is read, released, the miss computed, and the answer written back.
#[derive(Debug, Default)]
pub(super) struct GatherCache {
    addresses: Mutex<Option<(Instant, Vec<IpAddr>)>>,
    servers: Mutex<Option<(Instant, ResolvedServers)>>,
}

impl GatherCache {
    /// This host's offerable addresses. An empty answer is not cached, so a
    /// host that had no address retries on its next offer.
    pub(super) async fn host_addresses(&self) -> Vec<IpAddr> {
        self.host_addresses_at(Instant::now(), host_addresses::host_addresses)
            .await
    }

    /// The addresses `stun_urls` resolve to before `deadline`. An empty
    /// answer is not cached: a lookup that failed is retried next offer.
    pub(super) async fn servers(
        &self,
        stun_urls: &[String],
        deadline: tokio::time::Instant,
    ) -> Vec<SocketAddr> {
        self.servers_at(Instant::now(), stun_urls, || {
            resolve_servers(stun_urls, deadline)
        })
        .await
    }

    async fn host_addresses_at<Resolve, Resolving>(
        &self,
        now: Instant,
        resolve: Resolve,
    ) -> Vec<IpAddr>
    where
        Resolve: FnOnce() -> Resolving,
        Resolving: Future<Output = Vec<IpAddr>>,
    {
        if let Some(cached) = fresh(&self.addresses, now, |_| true) {
            return cached;
        }
        let addresses = resolve().await;
        if !addresses.is_empty() {
            store(&self.addresses, now, addresses.clone());
        }
        addresses
    }

    async fn servers_at<Resolve, Resolving>(
        &self,
        now: Instant,
        stun_urls: &[String],
        resolve: Resolve,
    ) -> Vec<SocketAddr>
    where
        Resolve: FnOnce() -> Resolving,
        Resolving: Future<Output = Vec<SocketAddr>>,
    {
        let same_urls = |cached: &ResolvedServers| cached.stun_urls == stun_urls;
        if let Some(cached) = fresh(&self.servers, now, same_urls) {
            return cached.servers;
        }
        let servers = resolve().await;
        if !servers.is_empty() {
            let resolved = ResolvedServers {
                stun_urls: stun_urls.to_vec(),
                servers: servers.clone(),
            };
            store(&self.servers, now, resolved);
        }
        servers
    }
}

/// The cached value when it is younger than the TTL and still answers the
/// question asked. A poisoned lock reads as a miss.
fn fresh<Value: Clone>(
    slot: &Mutex<Option<(Instant, Value)>>,
    now: Instant,
    answers: impl FnOnce(&Value) -> bool,
) -> Option<Value> {
    let held = slot.lock().ok()?;
    let (stored_at, value) = held.as_ref()?;
    let young = now.saturating_duration_since(*stored_at) < GATHER_CACHE_TTL;
    (young && answers(value)).then(|| value.clone())
}

fn store<Value>(slot: &Mutex<Option<(Instant, Value)>>, now: Instant, value: Value) {
    if let Ok(mut held) = slot.lock() {
        *held = Some((now, value));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::{GATHER_CACHE_TTL, GatherCache};

    fn address(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, last))
    }

    #[tokio::test]
    async fn a_second_offer_inside_the_ttl_reuses_the_addresses_and_a_later_one_resolves_again() {
        let cache = GatherCache::default();
        let calls = AtomicUsize::new(0);
        let resolve = |answer: IpAddr| {
            let calls = &calls;
            move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                vec![answer]
            }
        };
        let start = Instant::now();

        let first = cache.host_addresses_at(start, resolve(address(1))).await;
        let inside = start + GATHER_CACHE_TTL - Duration::from_millis(1);
        let second = cache.host_addresses_at(inside, resolve(address(2))).await;
        assert_eq!(first, vec![address(1)]);
        assert_eq!(second, vec![address(1)], "the cached answer was reused");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let after = start + GATHER_CACHE_TTL;
        let third = cache.host_addresses_at(after, resolve(address(3))).await;
        assert_eq!(
            third,
            vec![address(3)],
            "an expired answer was resolved again"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_empty_answer_is_not_cached() {
        let cache = GatherCache::default();
        let now = Instant::now();
        assert!(
            cache
                .host_addresses_at(now, || async { Vec::new() })
                .await
                .is_empty()
        );
        let retried = cache
            .host_addresses_at(now, || async { vec![address(4)] })
            .await;
        assert_eq!(retried, vec![address(4)]);
    }

    #[tokio::test]
    async fn resolved_servers_are_reused_only_for_the_same_urls_inside_the_ttl() {
        let cache = GatherCache::default();
        let calls = AtomicUsize::new(0);
        let server = |port: u16| SocketAddr::new(address(9), port);
        let urls = vec!["stun:stun.example:3478".to_owned()];
        let other = vec!["stun:other.example:3478".to_owned()];
        let start = Instant::now();
        let resolve = |answer: SocketAddr| {
            let calls = &calls;
            move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                vec![answer]
            }
        };

        let first = cache.servers_at(start, &urls, resolve(server(1))).await;
        let second = cache.servers_at(start, &urls, resolve(server(2))).await;
        assert_eq!(first, second);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let switched = cache.servers_at(start, &other, resolve(server(3))).await;
        assert_eq!(
            switched,
            vec![server(3)],
            "other URLs are resolved, not reused"
        );

        let after = start + GATHER_CACHE_TTL;
        let expired = cache.servers_at(after, &other, resolve(server(4))).await;
        assert_eq!(expired, vec![server(4)]);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
