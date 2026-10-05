//! Binds one UDP socket per local address and gathers the peer's candidates
//! before its answer is written: a host candidate per socket, then a
//! server-reflexive one per socket a STUN server answers before the gathering
//! deadline. Called by `peer::native::str0m_peer` from `answer`. Stands in for
//! libjuice's gathering inside node-datachannel for v2
//! `apps/worker/src/terminal/peer/terminal-peer-native.ts`.
//!
//! One socket per address, not one wildcard socket: str0m's ICE agent only
//! answers a check that arrived on a known local candidate address, and a
//! wildcard socket cannot say which address a datagram reached.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use str0m::Candidate;
use tokio::net::UdpSocket;
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout_at};

use super::gather_cache::{Egress, GatherCache};
use super::stun::{TransactionId, binding_request, mapped_address, new_transaction_id};
use super::{NativePeerConfig, NativePeerError};

/// RFC 5389's default STUN port, for a `stun:host` URL without one.
const DEFAULT_STUN_PORT: u16 = 3478;
/// How often an unanswered Binding request is sent again before the deadline.
const STUN_RESEND: Duration = Duration::from_millis(250);
/// How long one socket's probe runs. Four sends with no reply means the server
/// is unreachable from that address, and waiting out the full gathering
/// deadline for it delays every answer on a host with such an address.
const STUN_GIVE_UP: Duration = Duration::from_millis(1_000);
/// How long the other sockets get once one reflexive candidate exists: one
/// candidate a remote peer can reach is enough to connect, and a late sibling
/// only adds a pair.
const REFLEXIVE_SETTLE: Duration = Duration::from_millis(200);

#[derive(Debug)]
pub(super) struct Gathered {
    pub(super) sockets: Vec<Arc<UdpSocket>>,
    pub(super) candidates: Vec<Candidate>,
}

/// `cache` answers the host-address walk and the STUN lookup for every peer
/// the factory creates, so a burst of offers pays for each once.
pub(super) async fn gather(
    config: &NativePeerConfig,
    cache: &GatherCache,
    deadline: Instant,
) -> Result<Gathered, NativePeerError> {
    let addresses = match config.bind_address {
        Some(address) => vec![address],
        None => cache.host_addresses().await,
    };
    let mut sockets = Vec::with_capacity(addresses.len());
    let mut candidates = Vec::with_capacity(addresses.len());
    for address in addresses {
        let Some(socket) = bind_in_range(address, config.port_range).await else {
            continue;
        };
        let Ok(local) = socket.local_addr() else {
            continue;
        };
        match Candidate::host(local, "udp") {
            Ok(candidate) => {
                candidates.push(candidate);
                sockets.push(Arc::new(socket));
            }
            Err(error) => tracing::debug!(%error, "a local address is not a usable host candidate"),
        }
    }
    if sockets.is_empty() {
        tracing::warn!(peer = %config.name, "no local address could be bound for a peer");
        return Err(NativePeerError::NoLocalAddress);
    }
    let started = Instant::now();
    let reflexive = reflexive_candidates(&sockets, &config.stun_urls, cache, deadline).await;
    candidates.extend(reflexive);
    tracing::debug!(
        peer = %config.name,
        sockets = sockets.len(),
        candidates = candidates.len(),
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "the peer gathered its candidates"
    );
    Ok(Gathered {
        sockets,
        candidates,
    })
}

/// The first free port of the range on `address`, or an ephemeral one when
/// no range is configured.
async fn bind_in_range(address: IpAddr, range: Option<(u16, u16)>) -> Option<UdpSocket> {
    let Some((first, last)) = range else {
        return UdpSocket::bind((address, 0)).await.ok();
    };
    for port in first..=last {
        if let Ok(socket) = UdpSocket::bind((address, port)).await {
            return Some(socket);
        }
    }
    tracing::warn!(%address, first, last, "every port of the peer port range is taken");
    None
}

async fn reflexive_candidates(
    sockets: &[Arc<UdpSocket>],
    stun_urls: &[String],
    cache: &GatherCache,
    deadline: Instant,
) -> Vec<Candidate> {
    if stun_urls.is_empty() {
        return Vec::new();
    }
    let servers = cache.servers(stun_urls, deadline).await;
    let mut servers_with_egress = Vec::with_capacity(servers.len());
    for server in servers {
        servers_with_egress.push((server, cache.egress(server).await));
    }
    let probed: Vec<(&Arc<UdpSocket>, SocketAddr)> = sockets
        .iter()
        .filter_map(|socket| socket.local_addr().ok().map(|base| (socket, base)))
        .filter(|(_, base)| !base.ip().is_loopback())
        .collect();
    let bases: Vec<IpAddr> = probed.iter().map(|(_, base)| base.ip()).collect();
    let probe_deadline = deadline.min(Instant::now() + STUN_GIVE_UP);
    let mut probes = JoinSet::new();
    for ((socket, base), servers) in probed
        .iter()
        .zip(probe_targets(&bases, &servers_with_egress))
    {
        let targets: Vec<(SocketAddr, TransactionId)> = servers
            .into_iter()
            .filter_map(|server| new_transaction_id().ok().map(|id| (server, id)))
            .collect();
        if !targets.is_empty() {
            probes.spawn(probe_socket(
                Arc::clone(socket),
                *base,
                targets,
                probe_deadline,
            ));
        }
    }
    let mut found: Vec<Candidate> = Vec::new();
    let mut settle: Option<Instant> = None;
    loop {
        match timeout_at(settle.unwrap_or(deadline), probes.join_next()).await {
            Ok(Some(result)) => {
                for candidate in result.unwrap_or_default() {
                    if !found.iter().any(|known| known.addr() == candidate.addr()) {
                        found.push(candidate);
                    }
                }
                if !found.is_empty() && settle.is_none() {
                    settle = Some(Instant::now() + REFLEXIVE_SETTLE);
                }
            }
            Ok(None) => break,
            Err(_) => {
                probes.abort_all();
                break;
            }
        }
    }
    found
}

/// Which servers each host socket probes: only the ones its address is the
/// egress for, and none the routing table has no route to. A server whose
/// egress is none of the sockets (an operator `bind_address`) or could not be
/// read is probed by every socket of its family, as before, so a wrong guess
/// never costs a candidate. Probing from a socket that cannot reach the server
/// only holds the gathering open for the settle.
fn probe_targets(bases: &[IpAddr], servers: &[(SocketAddr, Egress)]) -> Vec<Vec<SocketAddr>> {
    bases
        .iter()
        .map(|base| {
            servers
                .iter()
                .filter(|(server, egress)| match egress {
                    Egress::Via(egress) if bases.contains(egress) => egress == base,
                    Egress::Unreachable => false,
                    Egress::Via(_) | Egress::Unknown => server.is_ipv4() == base.is_ipv4(),
                })
                .map(|(server, _)| *server)
                .collect()
        })
        .collect()
}

/// Every address a STUN URL's host resolves to before the deadline.
pub(super) async fn resolve_servers(stun_urls: &[String], deadline: Instant) -> Vec<SocketAddr> {
    let mut servers = Vec::new();
    for url in stun_urls {
        let Some((host, port)) = stun_host_port(url) else {
            continue;
        };
        match timeout_at(deadline, tokio::net::lookup_host((host.as_str(), port))).await {
            Ok(Ok(resolved)) => servers.extend(resolved),
            _ => tracing::debug!("a STUN server did not resolve before the gathering deadline"),
        }
    }
    servers
}

/// `stun:host`, `stun:host:port` or `stun:[v6]:port`, already normalized by
/// `parse_terminal_peer_stun_urls`.
fn stun_host_port(url: &str) -> Option<(String, u16)> {
    let authority = url.strip_prefix("stun:")?;
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, rest) = bracketed.split_once(']')?;
        let port = match rest.strip_prefix(':') {
            Some(port) => port.parse().ok()?,
            None => DEFAULT_STUN_PORT,
        };
        return Some((host.to_owned(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_owned(), port.parse().ok()?)),
        None => Some((authority.to_owned(), DEFAULT_STUN_PORT)),
    }
}

async fn probe_socket(
    socket: Arc<UdpSocket>,
    base: SocketAddr,
    mut pending: Vec<(SocketAddr, TransactionId)>,
    deadline: Instant,
) -> Vec<Candidate> {
    let mut found = Vec::new();
    let mut buffer = [0u8; 1500];
    let mut resend_at = Instant::now();
    while !pending.is_empty() && Instant::now() < deadline {
        if Instant::now() >= resend_at {
            for (server, transaction) in &pending {
                if let Err(error) = socket.send_to(&binding_request(transaction), server).await {
                    tracing::debug!(%error, "a STUN Binding request could not be sent");
                }
            }
            resend_at = Instant::now() + STUN_RESEND;
        }
        let received = timeout_at(resend_at.min(deadline), socket.recv_from(&mut buffer)).await;
        let Ok(Ok((length, from))) = received else {
            continue;
        };
        let datagram = &buffer[..length];
        let answered = pending
            .iter()
            .enumerate()
            .find_map(|(index, (server, transaction))| {
                (*server == from)
                    .then(|| mapped_address(datagram, transaction))
                    .flatten()
                    .map(|mapped| (index, mapped))
            });
        let Some((index, mapped)) = answered else {
            continue;
        };
        pending.swap_remove(index);
        if mapped == base {
            continue;
        }
        match Candidate::server_reflexive(mapped, base, "udp") {
            Ok(candidate) => found.push(candidate),
            Err(error) => tracing::debug!(%error, "a reflexive address is not a usable candidate"),
        }
    }
    found
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    use tokio::time::Instant;

    use super::super::gather_cache::{Egress, GatherCache};
    use super::super::{NativePeerConfig, host_addresses};
    use super::{gather, probe_targets, stun_host_port};

    /// A STUN server that never answers costs one give-up window, not the
    /// whole gathering deadline, and the host candidate is still offered.
    /// Needs a non-loopback IPv4 address, because loopback is never probed.
    #[tokio::test]
    async fn an_unanswering_stun_server_costs_the_give_up_window_not_the_deadline() {
        let Some(address) = host_addresses::host_addresses()
            .await
            .into_iter()
            .find(|address| address.is_ipv4() && !address.is_loopback())
        else {
            return;
        };
        let config = NativePeerConfig {
            name: "gather-test".to_owned(),
            // Port 1 on this host: nothing listens, so no Binding reply comes.
            stun_urls: vec!["stun:127.0.0.1:1".to_owned()],
            bind_address: Some(address),
            port_range: None,
            max_message_size: 1,
            channels: Vec::new(),
        };
        let started = Instant::now();
        let gathered = gather(
            &config,
            &GatherCache::default(),
            started + Duration::from_secs(3),
        )
        .await
        .unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(1_500),
            "gathering took {elapsed:?}"
        );
        assert_eq!(gathered.candidates.len(), 1, "the host candidate alone");
    }

    fn v4(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, last))
    }

    #[test]
    fn only_the_egress_socket_probes_a_server() {
        let tailnet = IpAddr::V4(Ipv4Addr::new(100, 66, 0, 2));
        let server = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(162, 159, 207, 0)), 3478);
        assert_eq!(
            probe_targets(&[v4(5), tailnet], &[(server, Egress::Via(v4(5)))]),
            vec![vec![server], Vec::new()]
        );
    }

    /// The shape that held every answer for the settle on a dual-stack server
    /// list: the v6 address has no route, and a tailnet v6 socket probed it.
    #[test]
    fn a_server_with_no_route_is_probed_by_no_socket() {
        let server = SocketAddr::new("2606:4700:49::".parse().unwrap(), 3478);
        let tailnet_v6: IpAddr = "fd7a:115c:a1e0::7632:c019".parse().unwrap();
        assert_eq!(
            probe_targets(&[v4(5), tailnet_v6], &[(server, Egress::Unreachable)]),
            vec![Vec::<SocketAddr>::new(), Vec::new()]
        );
    }

    #[test]
    fn with_no_egress_match_every_socket_probes_its_family() {
        let server = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(162, 159, 207, 0)), 3478);
        let v6: IpAddr = "2001:db8::5".parse().unwrap();
        let bases = [v4(5), v4(6), v6];
        let everyone = vec![vec![server], vec![server], Vec::new()];
        assert_eq!(
            probe_targets(&bases, &[(server, Egress::Unknown)]),
            everyone
        );
        let elsewhere = Egress::Via(IpAddr::V4(Ipv4Addr::new(192, 168, 9, 9)));
        assert_eq!(probe_targets(&bases, &[(server, elsewhere)]), everyone);
    }

    #[test]
    fn stun_urls_resolve_to_host_and_port() {
        assert_eq!(
            stun_host_port("stun:stun.example.org"),
            Some(("stun.example.org".into(), 3478))
        );
        assert_eq!(
            stun_host_port("stun:192.0.2.1:19302"),
            Some(("192.0.2.1".into(), 19302))
        );
        assert_eq!(
            stun_host_port("stun:[2001:db8::1]:5349"),
            Some(("2001:db8::1".into(), 5349))
        );
    }
}
